//! Budgeted tree-walking interpreter for the JavaScript sandbox subset.
//!
//! The interpreter is the only place where a script can observe runtime
//! behavior. It enforces every limit on each operation, checks the cancellation
//! token on every evaluation step, and exposes no capability beyond the pure
//! built-ins and the explicitly registered [`HostObject`](crate::js::HostObject)
//! values. All errors carry the source position of the node being evaluated.

use crate::CancelToken;
use crate::js::Limits;
use crate::js::acrobat::{AcrobatContext, EventData};
use crate::js::ast::{
    AssignOp, BinaryOp, Expr, ExprKind, Program, Stmt, StmtKind, UnaryOp, VarKind,
};
use crate::js::error::{JsError, Pos};
use crate::js::host::HostRegistry;
use crate::js::value::{BuiltinFn, Env, JsFunction, Value, format_number};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

/// Maximum nesting depth allowed while converting a value to a string.
const MAX_STRING_CONVERSION_DEPTH: usize = 32;

/// Control flow returned by statement execution.
#[derive(Clone, Debug)]
enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

/// A resolved assignment target.
///
/// Resolving a target once (before evaluating the right-hand side) means a
/// side-effecting index expression such as `items[next()] += 1` is evaluated
/// exactly once, and the same resolved key is used for the read and the write.
enum Place {
    Var(String),
    Property { receiver: Value, name: String },
    Index { receiver: Value, key: Value },
}

/// Executes a parsed [`Program`] under a fixed set of limits.
pub(crate) struct Interpreter<'a> {
    limits: &'a Limits,
    cancel: &'a CancelToken,
    host: &'a HostRegistry,
    acrobat: Option<&'a AcrobatContext>,
    steps: u64,
    calls: u64,
    call_depth: usize,
    total_iterations: u64,
    last_value: Value,
    start: Instant,
}

impl<'a> Interpreter<'a> {
    /// Creates an interpreter with fresh counters and an optional Acrobat
    /// document context.
    pub(crate) fn new_with_context(
        limits: &'a Limits,
        cancel: &'a CancelToken,
        host: &'a HostRegistry,
        acrobat: Option<&'a AcrobatContext>,
    ) -> Self {
        Self {
            limits,
            cancel,
            host,
            acrobat,
            steps: 0,
            calls: 0,
            call_depth: 0,
            total_iterations: 0,
            last_value: Value::Undefined,
            start: Instant::now(),
        }
    }

    /// Returns the injected event object, or a typed error when absent.
    fn event_ref(&self, pos: Pos) -> Result<std::rc::Rc<std::cell::RefCell<EventData>>, JsError> {
        self.acrobat
            .map(AcrobatContext::event_ref)
            .ok_or(JsError::NoAcrobatContext { pos })
    }

    /// Runs the program and returns the value of the last expression, or
    /// `undefined` for a program with no result.
    pub(crate) fn run(&mut self, program: &Program) -> Result<Value, JsError> {
        let env = self.global_env();
        let flow = self.exec_block(&program.body, &env, true)?;
        match flow {
            Flow::Return(value) => Ok(value),
            Flow::Normal | Flow::Break | Flow::Continue => Ok(self.last_value.clone()),
        }
    }

    fn global_env(&self) -> Env {
        let env = Env::root();
        let builtins = [
            ("Number", BuiltinFn::Number),
            ("String", BuiltinFn::String),
            ("Boolean", BuiltinFn::Boolean),
            ("parseInt", BuiltinFn::ParseInt),
            ("parseFloat", BuiltinFn::ParseFloat),
            ("isNaN", BuiltinFn::IsNaN),
        ];
        for (name, function) in builtins {
            let _ = env.declare(name, false, Value::BuiltinFn(function), Pos::default());
        }
        let _ = env.declare("Math", false, Value::MathNamespace, Pos::default());
        let _ = env.declare("util", false, Value::UtilNamespace, Pos::default());
        let acrobat_builtins = [
            ("AFNumber_Format", BuiltinFn::AfNumberFormat),
            ("AFNumber_Keystroke", BuiltinFn::AfNumberKeystroke),
            ("AFPercent_Format", BuiltinFn::AfPercentFormat),
            ("AFPercent_Keystroke", BuiltinFn::AfPercentKeystroke),
            ("AFDate_FormatEx", BuiltinFn::AfDateFormatEx),
            ("AFDate_KeystrokeEx", BuiltinFn::AfDateKeystrokeEx),
            ("AFDate_Format", BuiltinFn::AfDateFormat),
            ("AFDate_Keystroke", BuiltinFn::AfDateKeystroke),
        ];
        for (name, function) in acrobat_builtins {
            let _ = env.declare(name, false, Value::BuiltinFn(function), Pos::default());
        }
        if let Some(context) = self.acrobat {
            let _ = env.declare(
                "event",
                false,
                Value::AcrobatEvent(context.event_ref()),
                Pos::default(),
            );
        }
        for name in self.host.names() {
            if let Some(object) = self.host.get(name) {
                let _ = env.declare(name, false, Value::Host(Rc::clone(object)), Pos::default());
            }
        }
        env
    }

    fn tick(&mut self, pos: Pos) -> Result<(), JsError> {
        if self.cancel.is_cancelled() {
            return Err(JsError::Cancelled { pos });
        }
        self.steps += 1;
        if self.steps > self.limits.max_steps {
            return Err(JsError::StepBudgetExceeded {
                pos,
                limit: self.limits.max_steps,
            });
        }
        if let Some(budget) = self.limits.wall_clock
            && self.start.elapsed() > budget
        {
            return Err(JsError::WallClockExceeded {
                pos,
                limit_ms: budget.as_millis(),
            });
        }
        Ok(())
    }

    fn tick_loop(&mut self, pos: Pos, iterations: &mut u64) -> Result<(), JsError> {
        *iterations += 1;
        if *iterations > self.limits.max_loop_iterations {
            return Err(JsError::LoopIterationsExceeded {
                pos,
                limit: self.limits.max_loop_iterations,
            });
        }
        self.total_iterations += 1;
        if self.total_iterations > self.limits.max_total_iterations {
            return Err(JsError::TotalIterationsExceeded {
                pos,
                limit: self.limits.max_total_iterations,
            });
        }
        self.tick(pos)
    }

    fn enter_call(&mut self, pos: Pos) -> Result<(), JsError> {
        if self.call_depth + 1 > self.limits.max_call_depth {
            return Err(JsError::CallDepthExceeded {
                pos,
                limit: self.limits.max_call_depth,
            });
        }
        self.calls += 1;
        if self.calls > self.limits.max_calls {
            return Err(JsError::CallCountExceeded {
                pos,
                limit: self.limits.max_calls,
            });
        }
        self.call_depth += 1;
        self.tick(pos)
    }

    fn exec_block(&mut self, statements: &[Stmt], env: &Env, hoist: bool) -> Result<Flow, JsError> {
        if hoist {
            for statement in statements {
                if let StmtKind::FunctionDecl { name, params, body } = &statement.kind {
                    let function = Value::Function(Rc::new(JsFunction {
                        name: name.clone(),
                        params: params.clone(),
                        body: body.clone(),
                        env: env.clone(),
                    }));
                    env.declare(name, false, function, statement.pos)?;
                }
            }
        }
        for statement in statements {
            if matches!(statement.kind, StmtKind::FunctionDecl { .. }) {
                continue;
            }
            let flow = self.exec_stmt(statement, env)?;
            if !matches!(flow, Flow::Normal) {
                return Ok(flow);
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_stmt(&mut self, statement: &Stmt, env: &Env) -> Result<Flow, JsError> {
        self.tick(statement.pos)?;
        match &statement.kind {
            StmtKind::VarDecl { kind, name, init } => {
                let value = match init {
                    Some(expression) => self.eval(expression, env)?,
                    None => Value::Undefined,
                };
                env.declare(name, *kind != VarKind::Const, value, statement.pos)?;
                Ok(Flow::Normal)
            }
            StmtKind::Expr(expression) => {
                self.last_value = self.eval(expression, env)?;
                Ok(Flow::Normal)
            }
            StmtKind::Block(body) => self.exec_block(body, env, false),
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let condition = self.eval(cond, env)?;
                if truthy(&condition) {
                    self.exec_stmt(then_branch, env)
                } else if let Some(other) = else_branch {
                    self.exec_stmt(other, env)
                } else {
                    Ok(Flow::Normal)
                }
            }
            StmtKind::While { cond, body } => self.exec_while(cond, body, env, statement.pos),
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                let loop_env = Env::child(env);
                if let Some(initializer) = init {
                    self.exec_stmt(initializer, &loop_env)?;
                }
                let mut iterations = 0u64;
                loop {
                    if let Some(condition) = cond {
                        let value = self.eval(condition, &loop_env)?;
                        if !truthy(&value) {
                            break;
                        }
                    }
                    self.tick_loop(statement.pos, &mut iterations)?;
                    match self.exec_stmt(body, &loop_env)? {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        flow @ Flow::Return(_) => return Ok(flow),
                    }
                    if let Some(expression) = update {
                        self.eval(expression, &loop_env)?;
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::FunctionDecl { .. } => Ok(Flow::Normal),
            StmtKind::Return(value) => {
                let result = match value {
                    Some(expression) => self.eval(expression, env)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(result))
            }
            StmtKind::Break => Ok(Flow::Break),
            StmtKind::Continue => Ok(Flow::Continue),
            StmtKind::Empty => Ok(Flow::Normal),
        }
    }

    fn exec_while(
        &mut self,
        cond: &Expr,
        body: &Stmt,
        env: &Env,
        pos: Pos,
    ) -> Result<Flow, JsError> {
        let mut iterations = 0u64;
        loop {
            let condition = self.eval(cond, env)?;
            if !truthy(&condition) {
                break;
            }
            self.tick_loop(pos, &mut iterations)?;
            match self.exec_stmt(body, env)? {
                Flow::Normal | Flow::Continue => {}
                Flow::Break => break,
                flow @ Flow::Return(_) => return Ok(flow),
            }
        }
        Ok(Flow::Normal)
    }

    fn eval(&mut self, expression: &Expr, env: &Env) -> Result<Value, JsError> {
        self.tick(expression.pos)?;
        let pos = expression.pos;
        match &expression.kind {
            ExprKind::Number(value) => Ok(Value::Number(*value)),
            ExprKind::Str(value) => {
                Ok(Value::Str(checked_string(value.clone(), self.limits, pos)?))
            }
            ExprKind::Bool(value) => Ok(Value::Bool(*value)),
            ExprKind::Null => Ok(Value::Null),
            ExprKind::This => match self.acrobat {
                Some(context) => Ok(Value::DocumentContext(context.fields_ref())),
                None => Err(JsError::DangerousGlobal {
                    pos,
                    name: "this".to_owned(),
                }),
            },
            ExprKind::Ident(name) => {
                if crate::js::is_dangerous_name(name) {
                    return Err(JsError::DangerousGlobal {
                        pos,
                        name: name.clone(),
                    });
                }
                env.get(name, pos)
            }
            ExprKind::Unary { op, operand } => {
                let value = self.eval(operand, env)?;
                self.eval_unary(*op, &value, pos)
            }
            ExprKind::Binary { op, left, right } => {
                let lhs = self.eval(left, env)?;
                match op {
                    BinaryOp::And => {
                        if !truthy(&lhs) {
                            return Ok(lhs);
                        }
                        return self.eval(right, env);
                    }
                    BinaryOp::Or => {
                        if truthy(&lhs) {
                            return Ok(lhs);
                        }
                        return self.eval(right, env);
                    }
                    _ => {}
                }
                let rhs = self.eval(right, env)?;
                self.apply_binary(*op, lhs, rhs, pos)
            }
            ExprKind::Assign { op, target, value } => {
                // Resolve the target first so object/index expressions with side
                // effects are evaluated exactly once for the whole assignment.
                let place = self.resolve_place(target, env)?;
                let evaluated = self.eval(value, env)?;
                let result = if matches!(op, AssignOp::Assign) {
                    evaluated
                } else {
                    let current = self.read_place(&place, env, pos)?;
                    let binary = match op {
                        AssignOp::Add => BinaryOp::Add,
                        AssignOp::Sub => BinaryOp::Sub,
                        AssignOp::Mul => BinaryOp::Mul,
                        AssignOp::Div => BinaryOp::Div,
                        AssignOp::Mod => BinaryOp::Mod,
                        AssignOp::Assign => {
                            return Err(JsError::Internal {
                                pos,
                                message: "unreachable assignment operator".to_owned(),
                            });
                        }
                    };
                    self.apply_binary(binary, current, evaluated, pos)?
                };
                self.write_place(&place, env, result.clone(), pos)?;
                Ok(result)
            }
            ExprKind::Call { callee, args } => self.eval_call(callee, args, env, pos),
            ExprKind::Member { object, property } => {
                let receiver = self.eval(object, env)?;
                get_property(&receiver, property, pos)
            }
            ExprKind::Index { object, index } => {
                let receiver = self.eval(object, env)?;
                let index_value = self.eval(index, env)?;
                get_index(self, &receiver, &index_value, pos)
            }
            ExprKind::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(item, env)?);
                }
                check_array_len(values.len(), self.limits, pos)?;
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            ExprKind::Object(entries) => {
                if entries.len() > self.limits.max_object_entries {
                    return Err(JsError::ObjectTooLarge {
                        pos,
                        limit: self.limits.max_object_entries,
                    });
                }
                let mut map = BTreeMap::new();
                for (key, value_expr) in entries {
                    let value = self.eval(value_expr, env)?;
                    map.insert(key.clone(), value);
                }
                Ok(Value::Object(Rc::new(RefCell::new(map))))
            }
        }
    }

    fn resolve_place(&mut self, target: &Expr, env: &Env) -> Result<Place, JsError> {
        match &target.kind {
            ExprKind::Ident(name) => Ok(Place::Var(name.clone())),
            ExprKind::Member { object, property } => {
                let receiver = self.eval(object, env)?;
                Ok(Place::Property {
                    receiver,
                    name: property.clone(),
                })
            }
            ExprKind::Index { object, index } => {
                let receiver = self.eval(object, env)?;
                let key = self.eval(index, env)?;
                Ok(Place::Index { receiver, key })
            }
            _ => Err(JsError::Parse {
                pos: target.pos,
                message: "invalid assignment target".to_owned(),
            }),
        }
    }

    fn read_place(&mut self, place: &Place, env: &Env, pos: Pos) -> Result<Value, JsError> {
        match place {
            Place::Var(name) => env.get(name, pos),
            Place::Property { receiver, name } => get_property(receiver, name, pos),
            Place::Index { receiver, key } => get_index(self, receiver, key, pos),
        }
    }

    fn write_place(
        &mut self,
        place: &Place,
        env: &Env,
        value: Value,
        pos: Pos,
    ) -> Result<(), JsError> {
        match place {
            Place::Var(name) => env.set(name, value, pos),
            Place::Property { receiver, name } => self.set_property(receiver, name, value, pos),
            Place::Index { receiver, key } => set_index(self, receiver, key, value, pos),
        }
    }

    fn eval_unary(&self, op: UnaryOp, value: &Value, pos: Pos) -> Result<Value, JsError> {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!truthy(value))),
            UnaryOp::Neg => Ok(Value::Number(-to_number(value, self.limits, pos)?)),
            UnaryOp::Pos => Ok(Value::Number(to_number(value, self.limits, pos)?)),
        }
    }

    fn apply_binary(
        &mut self,
        op: BinaryOp,
        left: Value,
        right: Value,
        pos: Pos,
    ) -> Result<Value, JsError> {
        match op {
            BinaryOp::Add => {
                if matches!(left, Value::Str(_)) || matches!(right, Value::Str(_)) {
                    // Build the concatenation incrementally so the string-size
                    // budget is enforced while appending, never after a full
                    // `format!` allocation has already happened.
                    let mut out = String::new();
                    let mut len = 0usize;
                    let mut stack = Vec::new();
                    self.write_string(&left, &mut out, &mut len, &mut stack, pos)?;
                    self.write_string(&right, &mut out, &mut len, &mut stack, pos)?;
                    Ok(Value::Str(out))
                } else {
                    let a = to_number(&left, self.limits, pos)?;
                    let b = to_number(&right, self.limits, pos)?;
                    Ok(Value::Number(a + b))
                }
            }
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                let a = to_number(&left, self.limits, pos)?;
                let b = to_number(&right, self.limits, pos)?;
                if matches!(op, BinaryOp::Div | BinaryOp::Mod) && b == 0.0 {
                    return Err(JsError::DivisionByZero { pos });
                }
                let value = match op {
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => a / b,
                    BinaryOp::Mod => a % b,
                    _ => {
                        return Err(JsError::Internal {
                            pos,
                            message: "unreachable arithmetic operator".to_owned(),
                        });
                    }
                };
                Ok(Value::Number(value))
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let equal = loose_equal(&left, &right, self.limits, pos)?;
                Ok(Value::Bool(if matches!(op, BinaryOp::Ne) {
                    !equal
                } else {
                    equal
                }))
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let ordering = compare_values(&left, &right, self.limits, pos)?;
                let result = match op {
                    BinaryOp::Lt => ordering == Ordering::Less,
                    BinaryOp::Le => ordering != Ordering::Greater,
                    BinaryOp::Gt => ordering == Ordering::Greater,
                    BinaryOp::Ge => ordering != Ordering::Less,
                    _ => {
                        return Err(JsError::Internal {
                            pos,
                            message: "unreachable comparison operator".to_owned(),
                        });
                    }
                };
                Ok(Value::Bool(result))
            }
            BinaryOp::And | BinaryOp::Or => Err(JsError::Internal {
                pos,
                message: "logical operators must be evaluated lazily".to_owned(),
            }),
        }
    }

    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        env: &Env,
        pos: Pos,
    ) -> Result<Value, JsError> {
        if let ExprKind::Member { object, property } = &callee.kind {
            let receiver = self.eval(object, env)?;
            let values = self.eval_args(args, env)?;
            return self.call_method(&receiver, property, &values, pos);
        }
        let function = self.eval(callee, env)?;
        let values = self.eval_args(args, env)?;
        match function {
            Value::Function(reference) => self.call_function(&reference, values, pos),
            Value::BuiltinFn(builtin) => call_builtin(self, builtin, &values, pos),
            _ => Err(JsError::NotCallable { pos }),
        }
    }

    fn eval_args(&mut self, args: &[Expr], env: &Env) -> Result<Vec<Value>, JsError> {
        let mut values = Vec::with_capacity(args.len());
        for argument in args {
            values.push(self.eval(argument, env)?);
        }
        Ok(values)
    }

    fn call_method(
        &mut self,
        receiver: &Value,
        method: &str,
        args: &[Value],
        pos: Pos,
    ) -> Result<Value, JsError> {
        match receiver {
            Value::Str(text) => call_string_method(self, text, method, args, pos),
            Value::Array(items) => call_array_method(self, items, method, args, pos),
            Value::MathNamespace => {
                if let Some(builtin) = math_method(method) {
                    call_builtin(self, builtin, args, pos)
                } else {
                    Err(JsError::MethodNotFound {
                        pos,
                        name: method.to_owned(),
                    })
                }
            }
            Value::UtilNamespace => {
                if method == "printf" {
                    call_builtin(self, BuiltinFn::UtilPrintf, args, pos)
                } else {
                    Err(JsError::MethodNotFound {
                        pos,
                        name: method.to_owned(),
                    })
                }
            }
            Value::DocumentContext(fields) => {
                if method != "getField" {
                    return Err(JsError::MethodNotFound {
                        pos,
                        name: method.to_owned(),
                    });
                }
                let name = match args.first() {
                    Some(value) => self.stringify(value, pos)?,
                    None => String::new(),
                };
                Ok(Value::Field(Rc::clone(fields), name))
            }
            Value::Object(entries) => {
                let value = {
                    let entries = entries.try_borrow().map_err(|_| JsError::Internal {
                        pos,
                        message: "object was already mutably borrowed".to_owned(),
                    })?;
                    entries
                        .get(method)
                        .cloned()
                        .ok_or_else(|| JsError::MethodNotFound {
                            pos,
                            name: method.to_owned(),
                        })?
                };
                match value {
                    Value::Function(reference) => {
                        self.call_function(&reference, args.to_vec(), pos)
                    }
                    Value::BuiltinFn(builtin) => call_builtin(self, builtin, args, pos),
                    _ => Err(JsError::NotCallable { pos }),
                }
            }
            Value::Host(host) => host.call(method, args).map_err(|error| match error {
                JsError::Host { pos: p, message } if p == Pos::default() => {
                    JsError::Host { pos, message }
                }
                other => other,
            }),
            _ => Err(JsError::MethodNotFound {
                pos,
                name: method.to_owned(),
            }),
        }
    }

    fn call_function(
        &mut self,
        function: &JsFunction,
        args: Vec<Value>,
        pos: Pos,
    ) -> Result<Value, JsError> {
        self.enter_call(pos)?;
        let result = self.call_function_inner(function, args, pos);
        self.call_depth -= 1;
        result
    }

    fn call_function_inner(
        &mut self,
        function: &JsFunction,
        args: Vec<Value>,
        pos: Pos,
    ) -> Result<Value, JsError> {
        if args.len() != function.params.len() {
            return Err(JsError::Arity {
                pos,
                expected: function.params.len(),
                got: args.len(),
            });
        }
        let call_env = Env::child(&function.env);
        for (param, argument) in function.params.iter().zip(args) {
            call_env.declare(param, true, argument, pos)?;
        }
        let flow = self.exec_block(&function.body, &call_env, true)?;
        match flow {
            Flow::Return(value) => Ok(value),
            Flow::Normal | Flow::Break | Flow::Continue => Ok(Value::Undefined),
        }
    }

    /// Appends text to a string under construction, charging one CPU step and
    /// checking the size budget against the projected length *before* any
    /// allocation happens. This keeps large concatenations and joins bounded.
    fn push_string(
        &mut self,
        out: &mut String,
        length: &mut usize,
        text: &str,
        pos: Pos,
    ) -> Result<(), JsError> {
        self.tick(pos)?;
        let added = text.chars().count();
        let total = length.checked_add(added).ok_or(JsError::StringTooLarge {
            pos,
            limit: self.limits.max_string_len,
        })?;
        if total > self.limits.max_string_len {
            return Err(JsError::StringTooLarge {
                pos,
                limit: self.limits.max_string_len,
            });
        }
        out.push_str(text);
        *length = total;
        Ok(())
    }

    /// Converts a value to a string using a cycle-safe, depth-limited walk.
    fn stringify(&mut self, value: &Value, pos: Pos) -> Result<String, JsError> {
        let mut out = String::new();
        let mut length = 0usize;
        let mut visiting = Vec::new();
        self.write_string(value, &mut out, &mut length, &mut visiting, pos)?;
        Ok(out)
    }

    /// Recursive worker for [`Interpreter::stringify`].
    ///
    /// `visiting` holds the identities of arrays on the current path. A repeat
    /// identity is a cycle and is rejected with a typed error instead of
    /// recursing until the native stack overflows; the depth cap independently
    /// bounds deep but acyclic values.
    fn write_string(
        &mut self,
        value: &Value,
        out: &mut String,
        length: &mut usize,
        visiting: &mut Vec<usize>,
        pos: Pos,
    ) -> Result<(), JsError> {
        self.tick(pos)?;
        match value {
            Value::Str(text) => self.push_string(out, length, text, pos),
            Value::Number(number) => {
                let rendered = format_number(*number);
                self.push_string(out, length, &rendered, pos)
            }
            Value::Bool(boolean) => {
                self.push_string(out, length, if *boolean { "true" } else { "false" }, pos)
            }
            Value::Null => self.push_string(out, length, "null", pos),
            Value::Undefined => self.push_string(out, length, "undefined", pos),
            Value::Array(items) => {
                let id = Rc::as_ptr(items) as *const () as usize;
                if visiting.contains(&id) {
                    return Err(JsError::CyclicValue { pos });
                }
                if visiting.len() >= MAX_STRING_CONVERSION_DEPTH {
                    return Err(JsError::ValueTooDeep {
                        pos,
                        limit: MAX_STRING_CONVERSION_DEPTH,
                    });
                }
                visiting.push(id);
                let borrowed = items.try_borrow().map_err(|_| JsError::Internal {
                    pos,
                    message: "array was already mutably borrowed".to_owned(),
                })?;
                for (index, item) in borrowed.iter().enumerate() {
                    if index > 0 {
                        self.push_string(out, length, ",", pos)?;
                    }
                    self.write_string(item, out, length, visiting, pos)?;
                }
                visiting.pop();
                Ok(())
            }
            _ => Err(JsError::Type {
                pos,
                message: format!("cannot convert {} to string", value.type_name()),
            }),
        }
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Null | Value::Undefined => false,
        Value::Number(value) => *value != 0.0 && !value.is_nan(),
        Value::Str(value) => !value.is_empty(),
        Value::Array(_)
        | Value::Object(_)
        | Value::Function(_)
        | Value::BuiltinFn(_)
        | Value::MathNamespace
        | Value::UtilNamespace
        | Value::AcrobatEvent(_)
        | Value::DocumentContext(_)
        | Value::Field(_, _)
        | Value::Host(_) => true,
    }
}

fn to_number(value: &Value, _limits: &Limits, pos: Pos) -> Result<f64, JsError> {
    match value {
        Value::Number(value) => Ok(*value),
        Value::Bool(value) => Ok(if *value { 1.0 } else { 0.0 }),
        Value::Null | Value::Undefined => Ok(0.0),
        Value::Str(value) => Ok(value.trim().parse::<f64>().unwrap_or(f64::NAN)),
        _ => Err(JsError::Type {
            pos,
            message: format!(
                "expected a number, string, boolean, null, or undefined; got {}",
                value.type_name()
            ),
        }),
    }
}

fn checked_string(value: String, limits: &Limits, pos: Pos) -> Result<String, JsError> {
    if value.chars().count() > limits.max_string_len {
        return Err(JsError::StringTooLarge {
            pos,
            limit: limits.max_string_len,
        });
    }
    Ok(value)
}

fn check_array_len(length: usize, limits: &Limits, pos: Pos) -> Result<(), JsError> {
    if length > limits.max_array_len {
        return Err(JsError::ArrayTooLarge {
            pos,
            limit: limits.max_array_len,
        });
    }
    Ok(())
}

fn loose_equal(left: &Value, right: &Value, limits: &Limits, pos: Pos) -> Result<bool, JsError> {
    match (left, right) {
        (Value::Null, Value::Undefined)
        | (Value::Undefined, Value::Null)
        | (Value::Undefined, Value::Undefined)
        | (Value::Null, Value::Null) => Ok(true),
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        (Value::Number(a), Value::Number(b)) => Ok(a == b),
        (Value::Str(a), Value::Str(b)) => Ok(a == b),
        (Value::Number(_), Value::Str(_)) | (Value::Str(_), Value::Number(_)) => {
            Ok(to_number(left, limits, pos)? == to_number(right, limits, pos)?)
        }
        (Value::Bool(_), other) | (other, Value::Bool(_)) => {
            Ok(to_number(left, limits, pos)? == to_number(other, limits, pos)?)
        }
        _ => Ok(false),
    }
}

fn compare_values(
    left: &Value,
    right: &Value,
    limits: &Limits,
    pos: Pos,
) -> Result<Ordering, JsError> {
    let ordering = match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
        (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
        _ => {
            let a = to_number(left, limits, pos)?;
            let b = to_number(right, limits, pos)?;
            a.partial_cmp(&b)
        }
    };
    ordering.ok_or(JsError::Type {
        pos,
        message: "cannot compare values containing NaN".to_owned(),
    })
}

fn get_property(receiver: &Value, name: &str, pos: Pos) -> Result<Value, JsError> {
    match receiver {
        Value::Str(text) if name == "length" => Ok(Value::Number(text.chars().count() as f64)),
        Value::Array(items) if name == "length" => {
            let items = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            Ok(Value::Number(items.len() as f64))
        }
        Value::Object(entries) => {
            let entries = entries.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "object was already mutably borrowed".to_owned(),
            })?;
            entries
                .get(name)
                .cloned()
                .ok_or_else(|| JsError::PropertyNotFound {
                    pos,
                    name: name.to_owned(),
                })
        }
        Value::MathNamespace => {
            math_method(name)
                .map(Value::BuiltinFn)
                .ok_or_else(|| JsError::PropertyNotFound {
                    pos,
                    name: name.to_owned(),
                })
        }
        Value::UtilNamespace if name == "printf" => Ok(Value::BuiltinFn(BuiltinFn::UtilPrintf)),
        Value::AcrobatEvent(event) => {
            let event = event.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "event was already mutably borrowed".to_owned(),
            })?;
            match name {
                "value" => Ok(Value::Str(event.value.clone())),
                "key" => Ok(Value::Str(event.key.clone())),
                "rc" => Ok(Value::Bool(event.rc)),
                _ => Err(JsError::PropertyNotFound {
                    pos,
                    name: name.to_owned(),
                }),
            }
        }
        Value::Field(fields, field_name) => match name {
            "value" => {
                let fields = fields.try_borrow().map_err(|_| JsError::Internal {
                    pos,
                    message: "field store was already mutably borrowed".to_owned(),
                })?;
                Ok(Value::Str(fields.value(field_name)))
            }
            "name" => Ok(Value::Str(field_name.clone())),
            _ => Err(JsError::PropertyNotFound {
                pos,
                name: name.to_owned(),
            }),
        },
        _ => Err(JsError::PropertyNotFound {
            pos,
            name: name.to_owned(),
        }),
    }
}

impl<'a> Interpreter<'a> {
    fn set_property(
        &mut self,
        receiver: &Value,
        name: &str,
        value: Value,
        pos: Pos,
    ) -> Result<(), JsError> {
        match receiver {
            Value::Object(entries) => {
                let mut entries = entries.try_borrow_mut().map_err(|_| JsError::Internal {
                    pos,
                    message: "object was already borrowed".to_owned(),
                })?;
                if !entries.contains_key(name) && entries.len() >= self.limits.max_object_entries {
                    return Err(JsError::ObjectTooLarge {
                        pos,
                        limit: self.limits.max_object_entries,
                    });
                }
                entries.insert(name.to_owned(), value);
                Ok(())
            }
            Value::AcrobatEvent(event) => {
                let mut event = event.try_borrow_mut().map_err(|_| JsError::Internal {
                    pos,
                    message: "event was already borrowed".to_owned(),
                })?;
                match name {
                    "value" | "key" => {
                        let text = self.stringify(&value, pos)?;
                        if name == "value" {
                            event.value = text;
                        } else {
                            event.key = text;
                        }
                    }
                    "rc" => event.rc = truthy(&value),
                    _ => {
                        return Err(JsError::PropertyNotFound {
                            pos,
                            name: name.to_owned(),
                        });
                    }
                }
                Ok(())
            }
            Value::Field(fields, field_name) => {
                if name != "value" {
                    return Err(JsError::PropertyNotFound {
                        pos,
                        name: name.to_owned(),
                    });
                }
                let text = self.stringify(&value, pos)?;
                let mut fields = fields.try_borrow_mut().map_err(|_| JsError::Internal {
                    pos,
                    message: "field store was already borrowed".to_owned(),
                })?;
                fields.set(field_name.clone(), text);
                Ok(())
            }
            Value::Host(_) => Err(JsError::Host {
                pos,
                message: "host object properties are read-only; call a method instead".to_owned(),
            }),
            _ => Err(JsError::Type {
                pos,
                message: format!("cannot set property '{name}' on {}", receiver.type_name()),
            }),
        }
    }
}

fn index_number(value: &Value, pos: Pos) -> Result<i64, JsError> {
    match value {
        Value::Number(number) if number.fract() == 0.0 => Ok(*number as i64),
        Value::Str(text) => text.parse::<i64>().map_err(|_| JsError::Type {
            pos,
            message: format!("array index '{text}' is not an integer"),
        }),
        _ => Err(JsError::Type {
            pos,
            message: "array index must be an integer".to_owned(),
        }),
    }
}

fn get_index(
    interpreter: &mut Interpreter<'_>,
    receiver: &Value,
    index: &Value,
    pos: Pos,
) -> Result<Value, JsError> {
    match receiver {
        Value::Array(items) => {
            let index = index_number(index, pos)?;
            let items = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            if index < 0 {
                return Err(JsError::IndexOutOfBounds {
                    pos,
                    index,
                    len: items.len(),
                });
            }
            items
                .get(index as usize)
                .cloned()
                .ok_or_else(|| JsError::IndexOutOfBounds {
                    pos,
                    index,
                    len: items.len(),
                })
        }
        Value::Str(text) => {
            let index = index_number(index, pos)?;
            let chars: Vec<char> = text.chars().collect();
            if index < 0 || index as usize >= chars.len() {
                Ok(Value::Undefined)
            } else {
                Ok(Value::Str(chars[index as usize].to_string()))
            }
        }
        Value::Object(entries) => {
            let key = interpreter.stringify(index, pos)?;
            let entries = entries.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "object was already mutably borrowed".to_owned(),
            })?;
            entries
                .get(&key)
                .cloned()
                .ok_or(JsError::PropertyNotFound { pos, name: key })
        }
        _ => Err(JsError::Type {
            pos,
            message: format!("cannot index {}", receiver.type_name()),
        }),
    }
}

fn set_index(
    interpreter: &mut Interpreter<'_>,
    receiver: &Value,
    index: &Value,
    value: Value,
    pos: Pos,
) -> Result<(), JsError> {
    match receiver {
        Value::Array(items) => {
            let index = index_number(index, pos)?;
            if index < 0 {
                let len = items.try_borrow().map(|items| items.len()).unwrap_or(0);
                return Err(JsError::IndexOutOfBounds { pos, index, len });
            }
            let mut items = items.try_borrow_mut().map_err(|_| JsError::Internal {
                pos,
                message: "array was already borrowed".to_owned(),
            })?;
            let position = index as usize;
            if position > items.len() || position >= interpreter.limits.max_array_len {
                return Err(JsError::ArrayTooLarge {
                    pos,
                    limit: interpreter.limits.max_array_len,
                });
            }
            if position == items.len() {
                items.push(value);
            } else {
                items[position] = value;
            }
            Ok(())
        }
        Value::Object(entries) => {
            let key = interpreter.stringify(index, pos)?;
            let mut entries = entries.try_borrow_mut().map_err(|_| JsError::Internal {
                pos,
                message: "object was already borrowed".to_owned(),
            })?;
            if !entries.contains_key(&key) && entries.len() >= interpreter.limits.max_object_entries
            {
                return Err(JsError::ObjectTooLarge {
                    pos,
                    limit: interpreter.limits.max_object_entries,
                });
            }
            entries.insert(key, value);
            Ok(())
        }
        _ => Err(JsError::Type {
            pos,
            message: format!("cannot index-assign {}", receiver.type_name()),
        }),
    }
}

fn math_method(name: &str) -> Option<BuiltinFn> {
    match name {
        "abs" => Some(BuiltinFn::MathAbs),
        "floor" => Some(BuiltinFn::MathFloor),
        "ceil" => Some(BuiltinFn::MathCeil),
        "round" => Some(BuiltinFn::MathRound),
        "min" => Some(BuiltinFn::MathMin),
        "max" => Some(BuiltinFn::MathMax),
        "sqrt" => Some(BuiltinFn::MathSqrt),
        "pow" => Some(BuiltinFn::MathPow),
        _ => None,
    }
}

fn call_builtin(
    interpreter: &mut Interpreter<'_>,
    function: BuiltinFn,
    args: &[Value],
    pos: Pos,
) -> Result<Value, JsError> {
    match function {
        BuiltinFn::Number => {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Number(to_number(&value, interpreter.limits, pos)?))
        }
        BuiltinFn::String => {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            Ok(Value::Str(interpreter.stringify(&value, pos)?))
        }
        BuiltinFn::Boolean => Ok(Value::Bool(truthy(
            &args.first().cloned().unwrap_or(Value::Undefined),
        ))),
        BuiltinFn::ParseInt => {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let text = interpreter.stringify(&value, pos)?;
            Ok(Value::Number(parse_integer_prefix(&text)))
        }
        BuiltinFn::ParseFloat => {
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let text = interpreter.stringify(&value, pos)?;
            Ok(Value::Number(parse_float_prefix(&text)))
        }
        BuiltinFn::IsNaN => {
            let value = to_number(
                &args.first().cloned().unwrap_or(Value::Undefined),
                interpreter.limits,
                pos,
            )?;
            Ok(Value::Bool(value.is_nan()))
        }
        BuiltinFn::MathAbs => unary_math(args, interpreter.limits, pos, f64::abs),
        BuiltinFn::MathFloor => unary_math(args, interpreter.limits, pos, f64::floor),
        BuiltinFn::MathCeil => unary_math(args, interpreter.limits, pos, f64::ceil),
        BuiltinFn::MathRound => unary_math(args, interpreter.limits, pos, f64::round),
        BuiltinFn::MathSqrt => unary_math(args, interpreter.limits, pos, f64::sqrt),
        BuiltinFn::MathMin => fold_math(args, interpreter.limits, pos, true),
        BuiltinFn::MathMax => fold_math(args, interpreter.limits, pos, false),
        BuiltinFn::MathPow => {
            if args.len() != 2 {
                return Err(JsError::Arity {
                    pos,
                    expected: 2,
                    got: args.len(),
                });
            }
            let base = to_number(&args[0], interpreter.limits, pos)?;
            let exponent = to_number(&args[1], interpreter.limits, pos)?;
            Ok(Value::Number(base.powf(exponent)))
        }
        BuiltinFn::AfNumberFormat => mutate_event(interpreter, args, pos, af_number_format),
        BuiltinFn::AfNumberKeystroke => mutate_event(interpreter, args, pos, af_number_keystroke),
        BuiltinFn::AfPercentFormat => mutate_event(interpreter, args, pos, af_percent_format),
        BuiltinFn::AfPercentKeystroke => mutate_event(interpreter, args, pos, af_percent_keystroke),
        BuiltinFn::AfDateFormatEx => mutate_event(interpreter, args, pos, af_date_format_ex),
        BuiltinFn::AfDateKeystrokeEx => mutate_event(interpreter, args, pos, af_date_keystroke_ex),
        BuiltinFn::AfDateFormat => mutate_event(interpreter, args, pos, af_date_format),
        BuiltinFn::AfDateKeystroke => mutate_event(interpreter, args, pos, af_date_keystroke),
        BuiltinFn::UtilPrintf => util_printf(interpreter, args, pos),
    }
}

fn unary_math(
    args: &[Value],
    limits: &Limits,
    pos: Pos,
    operation: fn(f64) -> f64,
) -> Result<Value, JsError> {
    if args.len() != 1 {
        return Err(JsError::Arity {
            pos,
            expected: 1,
            got: args.len(),
        });
    }
    Ok(Value::Number(operation(to_number(&args[0], limits, pos)?)))
}

fn fold_math(args: &[Value], limits: &Limits, pos: Pos, minimum: bool) -> Result<Value, JsError> {
    let mut values = Vec::with_capacity(args.len());
    for value in args {
        values.push(to_number(value, limits, pos)?);
    }
    let Some(result) = values
        .into_iter()
        .reduce(if minimum { f64::min } else { f64::max })
    else {
        return Ok(Value::Number(if minimum {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }));
    };
    Ok(Value::Number(result))
}

fn parse_integer_prefix(text: &str) -> f64 {
    let trimmed = text.trim_start();
    let mut end = 0usize;
    for (index, ch) in trimmed.char_indices() {
        if index == 0 && (ch == '+' || ch == '-') {
            end = index + ch.len_utf8();
            continue;
        }
        if ch.is_ascii_digit() {
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return f64::NAN;
    }
    trimmed[..end]
        .parse::<i64>()
        .map(|value| value as f64)
        .unwrap_or(f64::NAN)
}

fn parse_float_prefix(text: &str) -> f64 {
    let trimmed = text.trim_start();
    let mut end = 0usize;
    let mut seen_digit = false;
    let mut seen_dot = false;
    for (index, ch) in trimmed.char_indices() {
        if index == 0 && (ch == '+' || ch == '-') {
            end = index + ch.len_utf8();
            continue;
        }
        if ch.is_ascii_digit() {
            seen_digit = true;
            end = index + ch.len_utf8();
        } else if ch == '.' && !seen_dot {
            seen_dot = true;
            end = index + ch.len_utf8();
        } else {
            break;
        }
    }
    if !seen_digit {
        return f64::NAN;
    }
    trimmed[..end].parse::<f64>().unwrap_or(f64::NAN)
}

fn arg_string(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    index: usize,
    pos: Pos,
) -> Result<String, JsError> {
    match args.get(index) {
        Some(value) => interpreter.stringify(value, pos),
        None => Ok(String::new()),
    }
}

fn arg_index(args: &[Value], index: usize, pos: Pos) -> Result<i64, JsError> {
    match args.get(index) {
        Some(value) => index_number(value, pos),
        None => Ok(0),
    }
}

fn char_slice(text: &str, start: i64, end: Option<i64>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let length = chars.len() as i64;
    let from = start.clamp(0, length) as usize;
    let to = end.unwrap_or(length).clamp(0, length) as usize;
    if from >= to {
        return String::new();
    }
    chars[from..to].iter().collect()
}

fn check_string_len(length: usize, limits: &Limits, pos: Pos) -> Result<(), JsError> {
    if length > limits.max_string_len {
        return Err(JsError::StringTooLarge {
            pos,
            limit: limits.max_string_len,
        });
    }
    Ok(())
}

fn call_string_method(
    interpreter: &mut Interpreter<'_>,
    text: &str,
    method: &str,
    args: &[Value],
    pos: Pos,
) -> Result<Value, JsError> {
    interpreter.tick(pos)?;
    match method {
        "toString" => Ok(Value::Str(checked_string(
            text.to_owned(),
            interpreter.limits,
            pos,
        )?)),
        "toUpperCase" | "toLowerCase" => {
            // Check the projected length before allocating the converted
            // string, since case conversion can expand individual characters.
            let projected: usize = if method == "toUpperCase" {
                text.chars().map(|ch| ch.to_uppercase().count()).sum()
            } else {
                text.chars().map(|ch| ch.to_lowercase().count()).sum()
            };
            check_string_len(projected, interpreter.limits, pos)?;
            let converted = if method == "toUpperCase" {
                text.to_uppercase()
            } else {
                text.to_lowercase()
            };
            Ok(Value::Str(converted))
        }
        "trim" => Ok(Value::Str(checked_string(
            text.trim().to_owned(),
            interpreter.limits,
            pos,
        )?)),
        "charAt" => {
            let index = arg_index(args, 0, pos)?;
            let chars: Vec<char> = text.chars().collect();
            if index < 0 || index as usize >= chars.len() {
                Ok(Value::Str(String::new()))
            } else {
                Ok(Value::Str(chars[index as usize].to_string()))
            }
        }
        "indexOf" => {
            let needle = arg_string(interpreter, args, 0, pos)?;
            // `length` and `charAt` use character offsets, so report a
            // character offset here rather than a UTF-8 byte offset.
            let found = match text.find(&needle) {
                Some(byte_index) => text[..byte_index].chars().count() as f64,
                None => -1.0,
            };
            Ok(Value::Number(found))
        }
        "slice" | "substring" => {
            let mut start = arg_index(args, 0, pos)?;
            let mut end = if args.len() > 1 {
                Some(arg_index(args, 1, pos)?)
            } else {
                None
            };
            if method == "substring"
                && let Some(candidate) = end
                && start > candidate
            {
                let original_start = start;
                start = candidate;
                end = Some(original_start);
            }
            Ok(Value::Str(checked_string(
                char_slice(text, start, end),
                interpreter.limits,
                pos,
            )?))
        }
        "split" => {
            let separator = arg_string(interpreter, args, 0, pos)?;
            let mut values = Vec::new();
            if separator.is_empty() {
                for ch in text.chars() {
                    check_array_len(values.len() + 1, interpreter.limits, pos)?;
                    interpreter.tick(pos)?;
                    values.push(Value::Str(ch.to_string()));
                }
            } else {
                for part in text.split(&separator) {
                    check_array_len(values.len() + 1, interpreter.limits, pos)?;
                    interpreter.tick(pos)?;
                    values.push(Value::Str(checked_string(
                        part.to_owned(),
                        interpreter.limits,
                        pos,
                    )?));
                }
            }
            Ok(Value::Array(Rc::new(RefCell::new(values))))
        }
        _ => Err(JsError::MethodNotFound {
            pos,
            name: method.to_owned(),
        }),
    }
}

fn call_array_method(
    interpreter: &mut Interpreter<'_>,
    items: &Rc<RefCell<Vec<Value>>>,
    method: &str,
    args: &[Value],
    pos: Pos,
) -> Result<Value, JsError> {
    interpreter.tick(pos)?;
    match method {
        "toString" | "join" => {
            let separator = if method == "join" {
                arg_string(interpreter, args, 0, pos)?
            } else {
                ",".to_owned()
            };
            let borrowed = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            let mut out = String::new();
            let mut length = 0usize;
            // Seed the cycle path with the receiver so `a[0] = a; a.join()`
            // fails immediately with a typed error instead of recursing.
            let mut visiting = vec![Rc::as_ptr(items) as *const () as usize];
            for (index, item) in borrowed.iter().enumerate() {
                if index > 0 {
                    interpreter.push_string(&mut out, &mut length, &separator, pos)?;
                }
                interpreter.write_string(item, &mut out, &mut length, &mut visiting, pos)?;
            }
            Ok(Value::Str(out))
        }
        "push" => {
            let mut items = items.try_borrow_mut().map_err(|_| JsError::Internal {
                pos,
                message: "array was already borrowed".to_owned(),
            })?;
            for argument in args {
                check_array_len(items.len() + 1, interpreter.limits, pos)?;
                items.push(argument.clone());
            }
            Ok(Value::Number(items.len() as f64))
        }
        "pop" => {
            let mut items = items.try_borrow_mut().map_err(|_| JsError::Internal {
                pos,
                message: "array was already borrowed".to_owned(),
            })?;
            Ok(items.pop().unwrap_or(Value::Undefined))
        }
        "slice" => {
            let start = arg_index(args, 0, pos)?;
            let end = if args.len() > 1 {
                Some(arg_index(args, 1, pos)?)
            } else {
                None
            };
            let items = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            let length = items.len() as i64;
            let from = start.clamp(0, length) as usize;
            let to = end.unwrap_or(length).clamp(0, length) as usize;
            let slice = if from >= to {
                Vec::new()
            } else {
                items[from..to].to_vec()
            };
            check_array_len(slice.len(), interpreter.limits, pos)?;
            Ok(Value::Array(Rc::new(RefCell::new(slice))))
        }
        "indexOf" => {
            let needle = args.first().cloned().unwrap_or(Value::Undefined);
            let items = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            let index = items.iter().position(|item| item == &needle);
            Ok(Value::Number(
                index.map(|index| index as f64).unwrap_or(-1.0),
            ))
        }
        "concat" => {
            let items = items.try_borrow().map_err(|_| JsError::Internal {
                pos,
                message: "array was already mutably borrowed".to_owned(),
            })?;
            let mut result = items.clone();
            for argument in args {
                match argument {
                    Value::Array(other) => {
                        let other = other.try_borrow().map_err(|_| JsError::Internal {
                            pos,
                            message: "array was already mutably borrowed".to_owned(),
                        })?;
                        for value in other.iter() {
                            check_array_len(result.len() + 1, interpreter.limits, pos)?;
                            result.push(value.clone());
                        }
                    }
                    other => {
                        check_array_len(result.len() + 1, interpreter.limits, pos)?;
                        result.push(other.clone());
                    }
                }
            }
            Ok(Value::Array(Rc::new(RefCell::new(result))))
        }
        "reverse" => {
            let mut items_mut = items.try_borrow_mut().map_err(|_| JsError::Internal {
                pos,
                message: "array was already borrowed".to_owned(),
            })?;
            items_mut.reverse();
            Ok(Value::Array(Rc::clone(items)))
        }
        _ => Err(JsError::MethodNotFound {
            pos,
            name: method.to_owned(),
        }),
    }
}

// --- Acrobat compatibility helpers -----------------------------------------
//
// The functions below implement the documented deterministic subsets of the
// Acrobat field format/validate API. They read and write only the injected
// in-memory [`EventData`]; they never touch a document, a clock, or the host.

fn mutate_event<F>(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    pos: Pos,
    mutate: F,
) -> Result<Value, JsError>
where
    F: FnOnce(&mut Interpreter<'_>, &[Value], &mut EventData, Pos) -> Result<Value, JsError>,
{
    interpreter.tick(pos)?;
    let event = interpreter.event_ref(pos)?;
    let mut data = event.try_borrow_mut().map_err(|_| JsError::Internal {
        pos,
        message: "event was already borrowed".to_owned(),
    })?;
    mutate(interpreter, args, &mut data, pos)
}

fn arg_int(args: &[Value], index: usize, default: i64) -> i64 {
    match args.get(index) {
        Some(Value::Number(number)) if number.is_finite() => *number as i64,
        Some(Value::Bool(boolean)) => i64::from(*boolean),
        Some(Value::Str(text)) => text.trim().parse::<i64>().unwrap_or(default),
        _ => default,
    }
}

fn arg_bool(args: &[Value], index: usize, default: bool) -> bool {
    match args.get(index) {
        Some(Value::Bool(boolean)) => *boolean,
        Some(Value::Number(number)) => *number != 0.0,
        Some(Value::Str(text)) => matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ),
        _ => default,
    }
}

fn af_number_format(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    format_number_common(interpreter, args, event, pos, false)
}

fn af_percent_format(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    format_number_common(interpreter, args, event, pos, true)
}

fn format_number_common(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
    percent: bool,
) -> Result<Value, JsError> {
    let decimals = arg_int(args, 0, 2).clamp(0, 12) as u32;
    let sep_style = arg_int(args, 1, 0);
    let neg_style = arg_int(args, 2, 0);
    let currency = if percent {
        String::new()
    } else {
        arg_string(interpreter, args, 4, pos)?
    };
    let prepend = !percent && arg_bool(args, 5, true);

    let mut number = event.value.trim().parse::<f64>().unwrap_or(f64::NAN);
    if !number.is_finite() {
        event.rc = true;
        return Ok(Value::Undefined);
    }
    if percent {
        number *= 100.0;
    }

    let scale = 10f64.powi(decimals as i32);
    let scaled_float = (number.abs() * scale).round();
    let scale_u = scale as u64;
    let scaled = if scaled_float >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled_float as u64
    };
    let integer = scaled / scale_u;
    let fraction = scaled % scale_u;
    let negative = number < 0.0;

    let mut body = String::new();
    write_grouped(integer, sep_style, &mut body);
    if decimals > 0 {
        body.push(if sep_style == 2 { ',' } else { '.' });
        body.push_str(&format!("{fraction:0width$}", width = decimals as usize));
    }

    let parentheses = negative && matches!(neg_style, 1 | 3);
    let lead = if negative {
        if parentheses { "(" } else { "-" }
    } else {
        ""
    };
    let trail = if parentheses { ")" } else { "" };

    let mut out = String::new();
    let mut length = 0usize;
    if prepend && !currency.is_empty() {
        interpreter.push_string(&mut out, &mut length, &currency, pos)?;
    }
    interpreter.push_string(&mut out, &mut length, lead, pos)?;
    interpreter.push_string(&mut out, &mut length, &body, pos)?;
    interpreter.push_string(&mut out, &mut length, trail, pos)?;
    if !prepend && !currency.is_empty() {
        interpreter.push_string(&mut out, &mut length, &currency, pos)?;
    }
    if percent {
        interpreter.push_string(&mut out, &mut length, "%", pos)?;
    }
    event.value = out;
    event.rc = true;
    Ok(Value::Undefined)
}

fn write_grouped(value: u64, sep_style: i64, out: &mut String) {
    let digits = format!("{value}");
    match sep_style {
        0 | 2 => {
            let separator = if sep_style == 0 { ',' } else { '.' };
            let len = digits.len();
            for (index, ch) in digits.chars().enumerate() {
                if index > 0 && (len - index) % 3 == 0 {
                    out.push(separator);
                }
                out.push(ch);
            }
        }
        _ => out.push_str(&digits),
    }
}

fn af_number_keystroke(
    _interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    _pos: Pos,
) -> Result<Value, JsError> {
    let decimals = arg_int(args, 0, 2).clamp(0, 12);
    let accepted = match event.key.chars().next() {
        None => true,
        Some(_ch) if event.key.chars().count() > 1 => event
            .key
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+' | ' ')),
        Some(ch) if ch.is_ascii_digit() => true,
        Some('.') => decimals > 0 && !event.value.contains('.'),
        Some(',' | '-' | '+' | ' ') => true,
        Some(_) => false,
    };
    event.rc = accepted;
    Ok(Value::Bool(accepted))
}

fn af_percent_keystroke(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    af_number_keystroke(interpreter, args, event, pos)
}

fn af_date_format_ex(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    let format = arg_string(interpreter, args, 0, pos)?;
    format_date_with(interpreter, &format, event, pos)
}

fn af_date_format(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    let format = legacy_date_format(arg_int(args, 0, 0));
    format_date_with(interpreter, format, event, pos)
}

fn af_date_keystroke_ex(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    let format = arg_string(interpreter, args, 0, pos)?;
    keystroke_date_with(&format, event, pos)
}

fn af_date_keystroke(
    _interpreter: &mut Interpreter<'_>,
    args: &[Value],
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    let format = legacy_date_format(arg_int(args, 0, 0));
    keystroke_date_with(format, event, pos)
}

fn legacy_date_format(code: i64) -> &'static str {
    match code {
        0 => "mm/dd/yyyy",
        1 => "mm/dd/yy",
        2 => "dd/mm/yyyy",
        3 => "dd/mm/yy",
        4 => "yyyy/mm/dd",
        5 => "yy/mm/dd",
        _ => "mm/dd/yyyy",
    }
}

fn digit_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

fn format_date_with(
    interpreter: &mut Interpreter<'_>,
    format: &str,
    event: &mut EventData,
    pos: Pos,
) -> Result<Value, JsError> {
    let runs = digit_runs(&event.value);
    if runs.len() < 3 {
        event.rc = true;
        return Ok(Value::Undefined);
    }
    let (year, month, day) = if runs[0].chars().count() == 4 {
        (runs[0].as_str(), runs[1].as_str(), runs[2].as_str())
    } else {
        (runs[2].as_str(), runs[0].as_str(), runs[1].as_str())
    };

    let mut out = String::new();
    let mut length = 0usize;
    let mut index = 0usize;
    while index < format.len() {
        let rest = &format[index..];
        let token = if rest.starts_with("yyyy") {
            Some((4usize, year, 4usize))
        } else if rest.starts_with("yy") {
            Some((2, year, 2))
        } else if rest.starts_with("mm") {
            Some((2, month, 2))
        } else if rest.starts_with("dd") {
            Some((2, day, 2))
        } else if rest.starts_with('m') {
            Some((1, month, 1))
        } else if rest.starts_with('d') {
            Some((1, day, 1))
        } else {
            None
        };
        match token {
            Some((advance, component, width)) => {
                let piece = fit_component(component, width);
                interpreter.push_string(&mut out, &mut length, &piece, pos)?;
                index += advance;
            }
            None => {
                let ch = rest.chars().next().unwrap_or('\0');
                if ch != '\0' {
                    interpreter.push_string(&mut out, &mut length, &ch.to_string(), pos)?;
                    index += ch.len_utf8();
                } else {
                    break;
                }
            }
        }
    }
    event.value = out;
    event.rc = true;
    Ok(Value::Undefined)
}

fn fit_component(value: &str, width: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() >= width {
        chars[chars.len() - width..].iter().collect()
    } else {
        let mut padded = "0".repeat(width - chars.len());
        padded.push_str(value);
        padded
    }
}

fn keystroke_date_with(format: &str, event: &mut EventData, _pos: Pos) -> Result<Value, JsError> {
    let accepted = event
        .key
        .chars()
        .all(|ch| ch.is_ascii_digit() || ch == ' ' || format.contains(ch));
    event.rc = accepted;
    Ok(Value::Bool(accepted))
}

fn util_printf(
    interpreter: &mut Interpreter<'_>,
    args: &[Value],
    pos: Pos,
) -> Result<Value, JsError> {
    interpreter.tick(pos)?;
    let format = arg_string(interpreter, args, 0, pos)?;
    let chars: Vec<char> = format.chars().collect();
    let mut out = String::new();
    let mut length = 0usize;
    let mut index = 0usize;
    let mut next_arg = 1usize;

    while index < chars.len() {
        if chars[index] != '%' {
            interpreter.push_string(&mut out, &mut length, &chars[index].to_string(), pos)?;
            index += 1;
            continue;
        }
        if index + 1 < chars.len() && chars[index + 1] == '%' {
            interpreter.push_string(&mut out, &mut length, "%", pos)?;
            index += 2;
            continue;
        }

        let mut cursor = index + 1;
        let mut left_align = false;
        let mut zero_pad = false;
        while cursor < chars.len() && matches!(chars[cursor], '-' | '0') {
            if chars[cursor] == '-' {
                left_align = true;
            } else {
                zero_pad = true;
            }
            cursor += 1;
        }
        let width_start = cursor;
        while cursor < chars.len() && chars[cursor].is_ascii_digit() {
            cursor += 1;
        }
        let width = chars[width_start..cursor]
            .iter()
            .collect::<String>()
            .parse::<usize>()
            .unwrap_or(0);
        let mut precision: Option<usize> = None;
        if cursor < chars.len() && chars[cursor] == '.' {
            cursor += 1;
            let precision_start = cursor;
            while cursor < chars.len() && chars[cursor].is_ascii_digit() {
                cursor += 1;
            }
            precision = Some(
                chars[precision_start..cursor]
                    .iter()
                    .collect::<String>()
                    .parse::<usize>()
                    .unwrap_or(0),
            );
        }
        if cursor >= chars.len() {
            interpreter.push_string(&mut out, &mut length, "%", pos)?;
            index += 1;
            continue;
        }
        let conversion = chars[cursor];
        cursor += 1;

        let value = args.get(next_arg).cloned().unwrap_or(Value::Undefined);
        next_arg += 1;
        let mut piece = match conversion {
            's' => interpreter.stringify(&value, pos)?,
            'd' | 'i' => {
                let number = to_number(&value, interpreter.limits, pos)?;
                if number.is_finite() {
                    format!("{}", number as i64)
                } else {
                    "0".to_owned()
                }
            }
            'f' | 'F' => {
                let number = to_number(&value, interpreter.limits, pos)?;
                let places = precision.unwrap_or(6);
                if number.is_finite() {
                    format!("{number:.places$}")
                } else {
                    "NaN".to_owned()
                }
            }
            _ => {
                let literal: String = chars[index..cursor].iter().collect();
                interpreter.push_string(&mut out, &mut length, &literal, pos)?;
                index = cursor;
                continue;
            }
        };

        let current = piece.chars().count();
        if width > current {
            let pad_count = width - current;
            let pad = if zero_pad && !left_align { '0' } else { ' ' };
            let padding: String = std::iter::repeat_n(pad, pad_count).collect();
            if left_align {
                piece.push_str(&padding);
            } else {
                piece = format!("{padding}{piece}");
            }
        }
        interpreter.push_string(&mut out, &mut length, &piece, pos)?;
        index = cursor;
    }
    Ok(Value::Str(out))
}
