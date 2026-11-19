//! Meaningful behavior tests for the JavaScript sandbox subset.

use super::{HostObject, HostRegistry, JsError, Limits, Pos, Script, Value, run_script};
use crate::CancelToken;
use std::rc::Rc;
use std::time::Duration;

fn eval(source: &str) -> Value {
    run_script(source, &Limits::default(), &CancelToken::new())
        .unwrap_or_else(|error| panic!("script `{source}` failed: {error}"))
}

fn number(source: &str) -> f64 {
    eval(source)
        .as_number()
        .unwrap_or_else(|| panic!("expected a number from `{source}`"))
}

fn error(source: &str) -> JsError {
    run_script(source, &Limits::default(), &CancelToken::new())
        .expect_err("expected the script to be rejected")
}

#[test]
fn arithmetic_precedence_and_associativity() {
    assert_eq!(number("1 + 2 * 3;"), 7.0);
    assert_eq!(number("(1 + 2) * 3;"), 9.0);
    assert_eq!(number("10 - 3 - 2;"), 5.0);
    assert_eq!(number("2 + 3 * 4 - 8 / 2;"), 10.0);
    assert_eq!(number("7 % 4;"), 3.0);
    assert_eq!(number("-2 + 5;"), 3.0);
    assert_eq!(number("2 * -3 + 10;"), 4.0);
}

#[test]
fn comparison_and_logical_precedence() {
    assert_eq!(eval("1 + 2 < 4;"), Value::Bool(true));
    assert_eq!(eval("2 * 4 >= 7;"), Value::Bool(true));
    assert_eq!(eval("true || false && false;"), Value::Bool(true));
    assert_eq!(eval("false && true || true;"), Value::Bool(true));
    assert_eq!(number("0 || 7;"), 7.0);
    assert_eq!(number("3 && 8;"), 8.0);
}

#[test]
fn logical_operators_short_circuit() {
    assert_eq!(eval("false && undeclared;"), Value::Bool(false));
    assert_eq!(eval("true || undeclared;"), Value::Bool(true));
    assert_eq!(number("'' || 5;"), 5.0);
}

#[test]
fn string_array_and_object_access() {
    assert_eq!(number("[10, 20, 30][1];"), 20.0);
    assert_eq!(number("'hello'.length;"), 5.0);
    assert_eq!(eval("'hello'[1];"), Value::Str("e".to_owned()));
    assert_eq!(eval("'hi'.toUpperCase();"), Value::Str("HI".to_owned()));
    assert_eq!(
        eval("'a,b'.split(',').join('-');"),
        Value::Str("a-b".to_owned())
    );

    assert_eq!(
        eval("var o = {x: 5, y: 'ok'}; o.y;"),
        Value::Str("ok".to_owned())
    );
    assert_eq!(number("var o = {x: 5}; o['x'];"), 5.0);
    assert_eq!(number("var o = {x: 1}; o.x = 7; o.x;"), 7.0);
    assert_eq!(number("var a = [1, 2]; a[0] = 9; a[0];"), 9.0);
    assert_eq!(number("var a = [1]; a.push(2); a.length;"), 2.0);
}

#[test]
fn builtins_and_math_helpers_are_whitelisted() {
    assert_eq!(number("parseInt('42px');"), 42.0);
    assert_eq!(number("parseFloat('3.5kg');"), 3.5);
    assert_eq!(number("Math.max(1, 9, 3);"), 9.0);
    assert_eq!(number("Math.floor(-1.2);"), -2.0);
    assert_eq!(number("Math.pow(2, 5);"), 32.0);
    assert_eq!(eval("String(12);"), Value::Str("12".to_owned()));
    assert_eq!(eval("isNaN('xyz');"), Value::Bool(true));
    assert_eq!(eval("Number('7');"), Value::Number(7.0));
}

#[test]
fn function_declarations_recursion_and_conditionals() {
    let source =
        "function fact(n) { if (n <= 1) { return 1; } else { return n * fact(n - 1); } } fact(5);";
    assert_eq!(number(source), 120.0);
    let source = "function add(a, b) { return a + b; } add(3, 4);";
    assert_eq!(number(source), 7.0);
}

#[test]
fn loops_honor_break_and_continue() {
    let source = "var s = 0; for (var i = 0; i < 10; i = i + 1) { if (i == 3) { continue; } if (i == 6) { break; } s += i; } s;";
    assert_eq!(number(source), 12.0);
    let source = "var i = 0; var s = 0; while (i < 5) { s += i; i = i + 1; } s;";
    assert_eq!(number(source), 10.0);
}

#[test]
fn loop_iteration_caps_are_enforced() {
    let limits = Limits::default().with_max_loop_iterations(5);
    let failure = run_script("while (true) {}", &limits, &CancelToken::new())
        .expect_err("infinite loop must be stopped");
    assert!(matches!(
        failure,
        JsError::LoopIterationsExceeded { limit: 5, .. }
    ));

    let limits = Limits::default().with_max_total_iterations(3);
    let failure = run_script("while (true) {}", &limits, &CancelToken::new())
        .expect_err("total iteration cap must be enforced");
    assert!(matches!(
        failure,
        JsError::TotalIterationsExceeded { limit: 3, .. }
    ));
}

#[test]
fn recursion_depth_and_call_count_caps_are_enforced() {
    let source = "function f(n) { return f(n + 1); } f(0);";
    let limits = Limits::default().with_max_call_depth(8);
    let failure = run_script(source, &limits, &CancelToken::new())
        .expect_err("unbounded recursion must be stopped");
    assert!(matches!(
        failure,
        JsError::CallDepthExceeded { limit: 8, .. }
    ));

    let limits = Limits::default().with_max_calls(5);
    let failure = run_script(source, &limits, &CancelToken::new())
        .expect_err("call count cap must be enforced");
    assert!(matches!(
        failure,
        JsError::CallCountExceeded { limit: 5, .. }
    ));
}

#[test]
fn step_budget_is_enforced() {
    let limits = Limits::default().with_max_steps(5);
    let failure = run_script(
        "var x = 1; var y = 2; var z = x + y; z;",
        &limits,
        &CancelToken::new(),
    )
    .expect_err("step budget must be enforced");
    assert!(matches!(
        failure,
        JsError::StepBudgetExceeded { limit: 5, .. }
    ));
}

#[test]
fn cancellation_is_observed_before_and_during_a_run() {
    let token = CancelToken::new();
    token.cancel();
    let failure = run_script("1 + 1;", &Limits::default(), &token)
        .expect_err("a pre-cancelled token must stop the script");
    assert!(matches!(failure, JsError::Cancelled { .. }));

    let token = Rc::new(CancelToken::new());
    let mut host = HostRegistry::new();
    host.register("ctl", Rc::new(CancellingHost(Rc::clone(&token))))
        .expect("registration should succeed");
    let script = Script::parse("ctl.stop(); var later = 1; later;").expect("valid script");
    let failure = script
        .run_with_host(&Limits::default(), &token, &host)
        .expect_err("cancellation from inside a host call must stop the script");
    assert!(matches!(failure, JsError::Cancelled { .. }));
}

#[test]
fn dangerous_globals_are_rejected() {
    for name in [
        "eval",
        "Function",
        "require",
        "import",
        "process",
        "globalThis",
        "window",
        "this",
        "arguments",
    ] {
        let source = format!("{name};");
        let failure = error(&source);
        assert!(
            matches!(failure, JsError::DangerousGlobal { .. }),
            "{source} produced {failure}"
        );
    }
}

#[test]
fn unsupported_syntax_is_rejected() {
    for source in [
        "var x = new Object();",
        "class A {}",
        "typeof x;",
        "var t = `hi`;",
        "var x = 1 === 1;",
        "delete obj.x;",
        "async function f() {}",
    ] {
        let failure = error(source);
        assert!(
            matches!(failure, JsError::UnsupportedSyntax { .. }),
            "{source} produced {failure}"
        );
    }
}

#[derive(Debug)]
struct Adder;

impl HostObject for Adder {
    fn call(&self, method: &str, args: &[Value]) -> Result<Value, JsError> {
        match method {
            "add" => {
                let a = args.first().and_then(Value::as_number).unwrap_or(0.0);
                let b = args.get(1).and_then(Value::as_number).unwrap_or(0.0);
                Ok(Value::Number(a + b))
            }
            _ => Err(JsError::Host {
                pos: Pos::default(),
                message: format!("unknown method '{method}'"),
            }),
        }
    }
}

#[derive(Debug)]
struct CancellingHost(Rc<CancelToken>);

impl HostObject for CancellingHost {
    fn call(&self, _method: &str, _args: &[Value]) -> Result<Value, JsError> {
        self.0.cancel();
        Ok(Value::Number(1.0))
    }
}

#[test]
fn host_object_calls_are_explicit_and_audited() {
    let mut host = HostRegistry::new();
    host.register("calc", Rc::new(Adder))
        .expect("registration should succeed");
    assert_eq!(host.names().collect::<Vec<_>>(), ["calc"]);

    let script = Script::parse("calc.add(2, 3);").expect("valid script");
    let value = script
        .run_with_host(&Limits::default(), &CancelToken::new(), &host)
        .expect("host call should succeed");
    assert_eq!(value.as_number(), Some(5.0));

    let mut dangerous = HostRegistry::new();
    let result = dangerous.register("process", Rc::new(Adder));
    assert!(matches!(result, Err(JsError::DangerousGlobal { .. })));
}

#[test]
fn host_errors_carry_the_call_position() {
    let mut host = HostRegistry::new();
    host.register("calc", Rc::new(Adder)).expect("registered");
    let script = Script::parse("var x = 1;\ncalc.nope();").expect("valid script");
    let failure = script
        .run_with_host(&Limits::default(), &CancelToken::new(), &host)
        .expect_err("unknown host method must fail");
    match failure {
        JsError::Host { pos, .. } => {
            assert_eq!(pos.line, 2);
            assert_eq!(pos.column, 1);
        }
        other => panic!("expected a host error, got {other}"),
    }
}

#[test]
fn errors_have_deterministic_source_positions() {
    let failure = error("var x = 1;\nvar y = missing;\n");
    match &failure {
        JsError::AmbientAccess { pos, name } => {
            assert_eq!(name, "missing");
            assert_eq!(*pos, Pos::new(2, 9, 19));
        }
        other => panic!("expected an ambient-access error, got {other}"),
    }
    assert!(failure.to_string().starts_with("2:9: "));
    let repeated = error("var x = 1;\nvar y = missing;\n");
    assert_eq!(failure.to_string(), repeated.to_string());
}

#[test]
fn unresolved_names_fail_because_there_are_no_ambient_globals() {
    let failure = error("undeclared;");
    assert!(
        matches!(&failure, JsError::AmbientAccess { name, .. } if name == "undeclared"),
        "unexpected error: {failure}"
    );
    assert_eq!(number("Number('7');"), 7.0);
}

#[test]
fn const_bindings_and_duplicate_declarations_are_rejected() {
    assert!(matches!(
        error("const x = 1; x = 2;"),
        JsError::AssignToConst { .. }
    ));
    assert!(matches!(
        error("var x = 1; var x = 2;"),
        JsError::DuplicateDeclaration { .. }
    ));
}

#[test]
fn size_limits_are_enforced() {
    let limits = Limits::default().with_max_string_len(3);
    let failure = run_script("'abcdef';", &limits, &CancelToken::new())
        .expect_err("string size limit must be enforced");
    assert!(matches!(failure, JsError::StringTooLarge { .. }));

    let limits = Limits::default().with_max_array_len(2);
    let failure = run_script("[1, 2, 3];", &limits, &CancelToken::new())
        .expect_err("array size limit must be enforced");
    assert!(matches!(failure, JsError::ArrayTooLarge { .. }));

    let limits = Limits::default().with_max_object_entries(1);
    let failure = run_script("var o = {a: 1, b: 2}; o;", &limits, &CancelToken::new())
        .expect_err("object size limit must be enforced");
    assert!(matches!(failure, JsError::ObjectTooLarge { .. }));
}

#[test]
fn division_by_zero_and_deep_expressions_are_rejected() {
    assert!(matches!(error("1 / 0;"), JsError::DivisionByZero { .. }));

    let limits = Limits::default().with_max_ast_depth(4);
    let failure = run_script("((((((1))))));", &limits, &CancelToken::new())
        .expect_err("nesting limit must be enforced");
    assert!(matches!(failure, JsError::AstTooDeep { .. }));

    let limits = Limits::default().with_wall_clock(Duration::from_millis(5));
    assert_eq!(limits.wall_clock, Some(Duration::from_millis(5)));
}

#[test]
fn scripts_are_reusable_and_deterministic() {
    let script =
        Script::parse("var total = 0; for (var i = 0; i < 4; i = i + 1) { total += i; } total;")
            .expect("valid script");
    let limits = Limits::default();
    let first = script
        .run(&limits, &CancelToken::new())
        .expect("first run succeeds");
    let second = script
        .run(&limits, &CancelToken::new())
        .expect("second run succeeds");
    assert_eq!(first, second);
    assert_eq!(first.as_number(), Some(6.0));
}

#[test]
fn compile_time_unsupported_keywords_are_rejected_even_as_names() {
    assert!(matches!(
        error("var new = 1;"),
        JsError::UnsupportedSyntax { .. }
    ));
    assert!(matches!(
        error("var process = 1;"),
        JsError::DangerousGlobal { .. }
    ));
}

// --- T-0062 security-review regressions -----------------------------------

#[test]
fn self_referential_arrays_are_rejected_by_string_conversion_paths() {
    let source = "var a = [1]; a[0] = a; String(a);";
    let failure = error(source);
    assert!(
        matches!(failure, JsError::CyclicValue { .. }),
        "String() produced {failure}"
    );

    let source = "var a = [1]; a[0] = a; a.join(',');";
    let failure = error(source);
    assert!(
        matches!(failure, JsError::CyclicValue { .. }),
        "join() produced {failure}"
    );

    let source = "var a = [1]; a[0] = a; a.toString();";
    let failure = error(source);
    assert!(
        matches!(failure, JsError::CyclicValue { .. }),
        "toString() produced {failure}"
    );

    let source = "var a = [1]; a[0] = a; '' + a;";
    let failure = error(source);
    assert!(
        matches!(failure, JsError::CyclicValue { .. }),
        "string + produced {failure}"
    );

    // The error still carries a deterministic source position.
    assert_eq!(failure.pos().line, 1);
}

#[test]
fn deeply_nested_values_are_rejected_without_stack_overflow() {
    let source = "var a = [0]; for (var i = 0; i < 40; i = i + 1) { a = [a]; } String(a);";
    let failure = error(source);
    assert!(
        matches!(failure, JsError::ValueTooDeep { .. }),
        "deep conversion produced {failure}"
    );
}

#[test]
fn string_conversion_charges_cpu_steps() {
    let source = "String([1,2,3,4,5,6,7,8,9,10]);";
    let failure = run_script(
        source,
        &Limits::default().with_max_steps(20),
        &CancelToken::new(),
    )
    .expect_err("conversion must consume steps");
    assert!(
        matches!(failure, JsError::StepBudgetExceeded { .. }),
        "unexpected error: {failure}"
    );
    // The same script succeeds with a generous budget, proving the failure is
    // the conversion work and not the script setup.
    assert_eq!(
        run_script(source, &Limits::default(), &CancelToken::new())
            .expect("generous budget succeeds")
            .as_str(),
        Some("1,2,3,4,5,6,7,8,9,10")
    );
}

#[test]
fn join_and_concat_enforce_string_size_incrementally() {
    let source = "var a = []; for (var i = 0; i < 100; i = i + 1) { a.push('0123456789012345678901234567890123456789'); } a.join('');";
    let failure = run_script(
        source,
        &Limits::default().with_max_string_len(1000),
        &CancelToken::new(),
    )
    .expect_err("join must respect the size budget");
    assert!(
        matches!(failure, JsError::StringTooLarge { .. }),
        "unexpected error: {failure}"
    );

    let failure = run_script(
        "'aaaa' + 'bbbb';",
        &Limits::default().with_max_string_len(4),
        &CancelToken::new(),
    )
    .expect_err("concat must respect the size budget");
    assert!(matches!(failure, JsError::StringTooLarge { .. }));

    let failure = run_script(
        "String([1,2,3]);",
        &Limits::default().with_max_string_len(2),
        &CancelToken::new(),
    )
    .expect_err("String(array) must respect the size budget");
    assert!(matches!(failure, JsError::StringTooLarge { .. }));
}

#[test]
fn string_indexes_are_char_offsets_and_substring_swaps_reversed_args() {
    assert_eq!(number("'éx'.length;"), 2.0);
    assert_eq!(eval("'éx'.charAt(1);"), Value::Str("x".to_owned()));
    assert_eq!(number("'ééx'.indexOf('x');"), 2.0);
    assert_eq!(number("'abc'.indexOf('z');"), -1.0);
    assert_eq!(
        eval("'abcdef'.substring(4, 1);"),
        Value::Str("bcd".to_owned())
    );
    assert_eq!(eval("'abcdef'.slice(1, 4);"), Value::Str("bcd".to_owned()));
}

#[test]
fn object_index_operations_use_the_active_limits() {
    let limits = Limits::default().with_max_string_len(2);
    let source = "var o = {}; o[[1,2,3]] = 1; o;";
    let failure = run_script(source, &limits, &CancelToken::new())
        .expect_err("object index-write must use active limits");
    assert!(
        matches!(failure, JsError::StringTooLarge { .. }),
        "unexpected error: {failure}"
    );

    let source = "var o = {}; o[[1,2,3]];";
    let failure = run_script(source, &limits, &CancelToken::new())
        .expect_err("object index-read must use active limits");
    assert!(
        matches!(failure, JsError::StringTooLarge { .. }),
        "unexpected error: {failure}"
    );
}

#[test]
fn compound_assignment_evaluates_index_and_object_expressions_once() {
    let source = "var a = [10, 20]; var calls = 0; function next() { calls = calls + 1; return 0; } a[next()] += 5; a[0] + calls * 100;";
    assert_eq!(number(source), 115.0);

    let source = "var o = {x: 1}; var calls = 0; function obj() { calls = calls + 1; return o; } obj().x += 2; o.x + calls * 100;";
    assert_eq!(number(source), 103.0);
}
