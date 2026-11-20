//! Scriptable operation registry with typed parameters and call plans.
//!
//! This module models the "CLI for every scriptable operation" contract at the
//! library level (no CLI crate is touched). An [`OperationRegistry`] enumerates
//! named operations, each with typed [`ParamDescriptor`]s, resolves a command
//! line into a validated [`CallPlan`], and produces a deterministic summary.
//! Resolution is pure: it never executes an operation, touches the filesystem,
//! or performs I/O.
//!
//! The built-in descriptors mirror [`crate::SUPPORTED_COMMANDS`] so the registry
//! covers every scriptable operation this crate exposes today.

use std::collections::BTreeMap;
use std::fmt;

/// The value type of a scriptable operation parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    /// A free-form string.
    String,
    /// A boolean spelled `true` or `false`.
    Bool,
    /// A signed base-10 integer.
    Integer,
    /// A path string. Validation of the path itself stays with the host.
    Path,
}

impl ParamType {
    /// Returns the stable machine-readable name of this type.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Bool => "bool",
            Self::Integer => "integer",
            Self::Path => "path",
        }
    }
}

impl fmt::Display for ParamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed descriptor for one operation parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDescriptor {
    /// Parameter name used in `name=value` arguments.
    pub name: &'static str,
    /// Expected value type.
    pub ty: ParamType,
    /// Whether the parameter must be supplied.
    pub required: bool,
}

impl ParamDescriptor {
    /// Creates a required parameter descriptor.
    pub const fn required(name: &'static str, ty: ParamType) -> Self {
        Self {
            name,
            ty,
            required: true,
        }
    }

    /// Creates an optional parameter descriptor.
    pub const fn optional(name: &'static str, ty: ParamType) -> Self {
        Self {
            name,
            ty,
            required: false,
        }
    }
}

/// A named operation and its parameter descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationDescriptor {
    /// Stable operation name, matching [`crate::SUPPORTED_COMMANDS`].
    pub name: &'static str,
    /// Parameters accepted by this operation.
    pub params: &'static [ParamDescriptor],
}

/// Built-in descriptors, one per [`crate::SUPPORTED_COMMANDS`] entry.
pub static DEFAULT_OPERATIONS: &[OperationDescriptor] = &[
    OperationDescriptor {
        name: "version",
        params: &[],
    },
    OperationDescriptor {
        name: "commands",
        params: &[],
    },
    OperationDescriptor {
        name: "list-actions",
        params: &[ParamDescriptor::optional("path", ParamType::Path)],
    },
    OperationDescriptor {
        name: "show-action",
        params: &[ParamDescriptor::required("name", ParamType::String)],
    },
    OperationDescriptor {
        name: "dry-run",
        params: &[
            ParamDescriptor::required("action", ParamType::String),
            ParamDescriptor::optional("input", ParamType::Path),
        ],
    },
    OperationDescriptor {
        name: "run-batch",
        params: &[
            ParamDescriptor::required("action", ParamType::String),
            ParamDescriptor::optional("input", ParamType::Path),
            ParamDescriptor::optional("overwrite", ParamType::Bool),
        ],
    },
    OperationDescriptor {
        name: "list-toolsets",
        params: &[],
    },
    OperationDescriptor {
        name: "show-toolset",
        params: &[ParamDescriptor::required("name", ParamType::String)],
    },
    OperationDescriptor {
        name: "validate-toolset",
        params: &[ParamDescriptor::required("name", ParamType::String)],
    },
    OperationDescriptor {
        name: "run-toolset",
        params: &[
            ParamDescriptor::required("name", ParamType::String),
            ParamDescriptor::optional("limit", ParamType::Integer),
        ],
    },
];

/// A typed argument value in a [`CallPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedValue {
    /// A string value.
    String(String),
    /// A boolean value.
    Bool(bool),
    /// An integer value.
    Integer(i64),
    /// A path value.
    Path(String),
}

impl TypedValue {
    /// Returns the parameter type of this value.
    pub const fn ty(&self) -> ParamType {
        match self {
            Self::String(_) => ParamType::String,
            Self::Bool(_) => ParamType::Bool,
            Self::Integer(_) => ParamType::Integer,
            Self::Path(_) => ParamType::Path,
        }
    }
}

impl fmt::Display for TypedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) | Self::Path(value) => f.write_str(value),
            Self::Bool(value) => f.write_str(if *value { "true" } else { "false" }),
            Self::Integer(value) => write!(f, "{value}"),
        }
    }
}

/// A validated, typed plan to call one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallPlan {
    operation: String,
    arguments: BTreeMap<String, TypedValue>,
}

impl CallPlan {
    /// Returns the resolved operation name.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Returns arguments in deterministic alphabetical key order.
    pub fn arguments(&self) -> impl ExactSizeIterator<Item = (&str, &TypedValue)> {
        self.arguments
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// Returns a typed argument by name.
    pub fn argument(&self, key: &str) -> Option<&TypedValue> {
        self.arguments.get(key)
    }

    /// Returns a stable one-line rendering of this plan.
    pub fn summary(&self) -> String {
        let mut out = self.operation.clone();
        for (key, value) in &self.arguments {
            out.push('\t');
            out.push_str(key);
            out.push('=');
            out.push_str(&value.to_string());
        }
        out
    }
}

/// A typed reason command resolution failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No arguments were supplied.
    EmptyArguments,
    /// Two operations shared a name during registration.
    DuplicateOperation {
        /// The duplicated name.
        name: String,
    },
    /// The operation name was not registered.
    UnknownOperation {
        /// The unknown name.
        name: String,
    },
    /// An argument did not carry a `name=value` pair.
    MalformedArgument {
        /// The offending argument.
        value: String,
    },
    /// An argument named a parameter the operation does not accept.
    UnknownParameter {
        /// Operation name.
        operation: String,
        /// Unknown parameter name.
        parameter: String,
    },
    /// The same parameter was supplied more than once.
    DuplicateParameter {
        /// The duplicated parameter name.
        parameter: String,
    },
    /// A required parameter was missing.
    MissingParameter {
        /// Operation name.
        operation: String,
        /// Missing parameter name.
        parameter: String,
    },
    /// A boolean value was not `true` or `false`.
    InvalidBool {
        /// Parameter name.
        parameter: String,
        /// Offending value.
        value: String,
    },
    /// An integer value could not be parsed.
    InvalidInteger {
        /// Parameter name.
        parameter: String,
        /// Offending value.
        value: String,
    },
}

impl ResolveError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyArguments => "empty_arguments",
            Self::DuplicateOperation { .. } => "duplicate_operation",
            Self::UnknownOperation { .. } => "unknown_operation",
            Self::MalformedArgument { .. } => "malformed_argument",
            Self::UnknownParameter { .. } => "unknown_parameter",
            Self::DuplicateParameter { .. } => "duplicate_parameter",
            Self::MissingParameter { .. } => "missing_parameter",
            Self::InvalidBool { .. } => "invalid_bool",
            Self::InvalidInteger { .. } => "invalid_integer",
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyArguments => f.write_str("no command arguments supplied"),
            Self::DuplicateOperation { name } => {
                write!(f, "operation '{name}' is already registered")
            }
            Self::UnknownOperation { name } => write!(f, "unknown operation '{name}'"),
            Self::MalformedArgument { value } => {
                write!(f, "malformed argument '{value}'; expected name=value")
            }
            Self::UnknownParameter {
                operation,
                parameter,
            } => {
                write!(f, "operation '{operation}' has no parameter '{parameter}'")
            }
            Self::DuplicateParameter { parameter } => {
                write!(f, "parameter '{parameter}' was supplied more than once")
            }
            Self::MissingParameter {
                operation,
                parameter,
            } => {
                write!(
                    f,
                    "operation '{operation}' requires parameter '{parameter}'"
                )
            }
            Self::InvalidBool { parameter, value } => {
                write!(
                    f,
                    "parameter '{parameter}' expects true or false, got '{value}'"
                )
            }
            Self::InvalidInteger { parameter, value } => {
                write!(
                    f,
                    "parameter '{parameter}' expects an integer, got '{value}'"
                )
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// A deterministic registry of scriptable operations.
#[derive(Debug)]
pub struct OperationRegistry {
    operations: BTreeMap<String, OperationDescriptor>,
}

impl OperationRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            operations: BTreeMap::new(),
        }
    }

    /// Creates a registry preloaded with [`DEFAULT_OPERATIONS`].
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        for descriptor in DEFAULT_OPERATIONS {
            let _ = registry.register(*descriptor);
        }
        registry
    }

    /// Registers an operation, rejecting a duplicate name.
    pub fn register(&mut self, descriptor: OperationDescriptor) -> Result<(), ResolveError> {
        if self.operations.contains_key(descriptor.name) {
            return Err(ResolveError::DuplicateOperation {
                name: descriptor.name.to_owned(),
            });
        }
        self.operations
            .insert(descriptor.name.to_owned(), descriptor);
        Ok(())
    }

    /// Returns operation descriptors in deterministic name order.
    pub fn operations(&self) -> impl ExactSizeIterator<Item = &OperationDescriptor> {
        self.operations.values()
    }

    /// Returns operation names in deterministic alphabetical order.
    pub fn names(&self) -> Vec<&str> {
        self.operations.keys().map(String::as_str).collect()
    }

    /// Looks up an operation descriptor by name.
    pub fn describe(&self, name: &str) -> Option<&OperationDescriptor> {
        self.operations.get(name)
    }

    /// Returns the number of registered operations.
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Returns `true` when the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Resolves command arguments into a validated, typed [`CallPlan`].
    ///
    /// `args[0]` is the operation name; remaining entries must be `name=value`
    /// pairs. Required parameters must be present, values are parsed according
    /// to their descriptor's [`ParamType`], and keys are emitted alphabetically
    /// in the resulting plan.
    pub fn resolve(&self, args: &[String]) -> Result<CallPlan, ResolveError> {
        let (name, rest) = args.split_first().ok_or(ResolveError::EmptyArguments)?;
        let descriptor = self
            .operations
            .get(name.as_str())
            .ok_or_else(|| ResolveError::UnknownOperation { name: name.clone() })?;

        let mut arguments: BTreeMap<String, TypedValue> = BTreeMap::new();
        for raw in rest {
            let (key, value) = raw
                .split_once('=')
                .ok_or_else(|| ResolveError::MalformedArgument { value: raw.clone() })?;
            if key.is_empty() {
                return Err(ResolveError::MalformedArgument { value: raw.clone() });
            }
            let param = descriptor
                .params
                .iter()
                .find(|param| param.name == key)
                .ok_or_else(|| ResolveError::UnknownParameter {
                    operation: descriptor.name.to_owned(),
                    parameter: key.to_owned(),
                })?;
            if arguments.contains_key(key) {
                return Err(ResolveError::DuplicateParameter {
                    parameter: key.to_owned(),
                });
            }
            let typed = parse_value(param, value)?;
            arguments.insert(key.to_owned(), typed);
        }

        for param in descriptor.params {
            if param.required && !arguments.contains_key(param.name) {
                return Err(ResolveError::MissingParameter {
                    operation: descriptor.name.to_owned(),
                    parameter: param.name.to_owned(),
                });
            }
        }

        Ok(CallPlan {
            operation: descriptor.name.to_owned(),
            arguments,
        })
    }
}

impl Default for OperationRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

fn parse_value(param: &ParamDescriptor, value: &str) -> Result<TypedValue, ResolveError> {
    let parsed =
        match param.ty {
            ParamType::String => TypedValue::String(value.to_owned()),
            ParamType::Path => TypedValue::Path(value.to_owned()),
            ParamType::Bool => match value {
                "true" => TypedValue::Bool(true),
                "false" => TypedValue::Bool(false),
                other => {
                    return Err(ResolveError::InvalidBool {
                        parameter: param.name.to_owned(),
                        value: other.to_owned(),
                    });
                }
            },
            ParamType::Integer => TypedValue::Integer(value.parse::<i64>().map_err(|_| {
                ResolveError::InvalidInteger {
                    parameter: param.name.to_owned(),
                    value: value.to_owned(),
                }
            })?),
        };
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn enumeration_is_sorted_and_deterministic() {
        let registry = OperationRegistry::with_defaults();
        let names = registry.names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let mut expected = crate::SUPPORTED_COMMANDS.to_vec();
        expected.sort_unstable();
        assert_eq!(names, expected);
        assert_eq!(registry.len(), crate::SUPPORTED_COMMANDS.len());
        let other = OperationRegistry::with_defaults();
        assert_eq!(registry.names(), other.names());
        for operation in registry.operations() {
            assert!(
                crate::SUPPORTED_COMMANDS.contains(&operation.name),
                "{}",
                operation.name
            );
        }
    }

    #[test]
    fn descriptors_carry_typed_parameters() {
        let registry = OperationRegistry::with_defaults();
        let run_batch = registry.describe("run-batch").unwrap();
        let types: Vec<(ParamType, bool)> = run_batch
            .params
            .iter()
            .map(|p| (p.ty, p.required))
            .collect();
        assert_eq!(
            types,
            [
                (ParamType::String, true),
                (ParamType::Path, false),
                (ParamType::Bool, false),
            ]
        );
        assert_eq!(ParamType::Integer.as_str(), "integer");
    }

    #[test]
    fn unknown_operation_is_typed() {
        let registry = OperationRegistry::with_defaults();
        assert_eq!(
            registry.resolve(&args(&["bogus"])),
            Err(ResolveError::UnknownOperation {
                name: "bogus".to_owned()
            })
        );
    }

    #[test]
    fn missing_required_parameter_is_rejected() {
        let registry = OperationRegistry::with_defaults();
        assert_eq!(
            registry.resolve(&args(&["show-action"])),
            Err(ResolveError::MissingParameter {
                operation: "show-action".to_owned(),
                parameter: "name".to_owned(),
            })
        );
    }

    #[test]
    fn invalid_bool_and_integer_are_rejected() {
        let registry = OperationRegistry::with_defaults();
        assert_eq!(
            registry.resolve(&args(&["run-batch", "action=x", "overwrite=yes"])),
            Err(ResolveError::InvalidBool {
                parameter: "overwrite".to_owned(),
                value: "yes".to_owned(),
            })
        );
        assert_eq!(
            registry.resolve(&args(&["run-toolset", "name=x", "limit=many"])),
            Err(ResolveError::InvalidInteger {
                parameter: "limit".to_owned(),
                value: "many".to_owned(),
            })
        );
    }

    #[test]
    fn unknown_and_duplicate_parameters_are_rejected() {
        let registry = OperationRegistry::with_defaults();
        assert_eq!(
            registry.resolve(&args(&["show-action", "name=x", "other=y"])),
            Err(ResolveError::UnknownParameter {
                operation: "show-action".to_owned(),
                parameter: "other".to_owned(),
            })
        );
        assert_eq!(
            registry.resolve(&args(&["show-action", "name=x", "name=y"])),
            Err(ResolveError::DuplicateParameter {
                parameter: "name".to_owned()
            })
        );
        assert_eq!(
            registry.resolve(&args(&["show-action", "novalue"])),
            Err(ResolveError::MalformedArgument {
                value: "novalue".to_owned()
            })
        );
    }

    #[test]
    fn empty_inputs_are_rejected() {
        let registry = OperationRegistry::with_defaults();
        assert_eq!(registry.resolve(&[]), Err(ResolveError::EmptyArguments));
        let empty = OperationRegistry::new();
        assert!(empty.is_empty());
        assert_eq!(
            empty.resolve(&args(&["version"])),
            Err(ResolveError::UnknownOperation {
                name: "version".to_owned()
            })
        );
    }

    #[test]
    fn resolve_produces_a_deterministic_typed_call_plan() {
        let registry = OperationRegistry::with_defaults();
        let cmd = args(&[
            "run-batch",
            "input=in.pdf",
            "overwrite=true",
            "action=nightly",
        ]);
        let first = registry.resolve(&cmd).unwrap();
        let second = registry.resolve(&cmd).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.operation(), "run-batch");
        assert_eq!(
            first.argument("action"),
            Some(&TypedValue::String("nightly".to_owned()))
        );
        assert_eq!(first.argument("overwrite"), Some(&TypedValue::Bool(true)));
        assert_eq!(
            first.argument("input"),
            Some(&TypedValue::Path("in.pdf".to_owned()))
        );
        let keys: Vec<&str> = first.arguments().map(|(key, _)| key).collect();
        assert_eq!(keys, ["action", "input", "overwrite"]);
        assert_eq!(
            first.summary(),
            "run-batch\taction=nightly\tinput=in.pdf\toverwrite=true"
        );
        assert_eq!(second.summary(), first.summary());
    }

    #[test]
    fn integer_and_negative_values_parse() {
        let registry = OperationRegistry::with_defaults();
        let plan = registry
            .resolve(&args(&["run-toolset", "name=x", "limit=-7"]))
            .unwrap();
        assert_eq!(plan.argument("limit"), Some(&TypedValue::Integer(-7)));
        assert_eq!(plan.summary(), "run-toolset\tlimit=-7\tname=x");
    }

    #[test]
    fn no_argument_operations_are_valid() {
        let registry = OperationRegistry::with_defaults();
        let plan = registry.resolve(&args(&["version"])).unwrap();
        assert_eq!(plan.arguments().len(), 0);
        assert_eq!(plan.summary(), "version");
    }

    #[test]
    fn duplicate_registration_is_typed() {
        let mut registry = OperationRegistry::new();
        registry
            .register(OperationDescriptor {
                name: "x",
                params: &[],
            })
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            registry.register(OperationDescriptor {
                name: "x",
                params: &[]
            }),
            Err(ResolveError::DuplicateOperation {
                name: "x".to_owned()
            })
        );
    }

    #[test]
    fn errors_are_actionable() {
        let error = ResolveError::UnknownOperation {
            name: "bogus".to_owned(),
        };
        assert_eq!(error.code(), "unknown_operation");
        assert!(error.to_string().contains("bogus"));
    }
}
