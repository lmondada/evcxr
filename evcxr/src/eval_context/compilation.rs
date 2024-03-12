//! Compile Rust code into a shared library for later use.
//! 
//! This takes bits and pieces of `eval_context.rs` (parent module) and remashes
//! them together to separate code compilation from code execution.

use std::path::PathBuf;

use crate::{
    code_block::CodeBlock, eval_context::{Config, ContextState}, module::Module, rust_analyzer::RustAnalyzer, Error
};

use super::{create_initial_config, VariableMoveState, VariableState};

#[derive(Debug, Clone)]
pub struct FunctionArg {
    arg_name: String,
    arg_type: String,
}

#[derive(Debug, Clone)]
pub struct ParsedFunction {
    name: String,
    fn_body: String,
    inputs: Vec<FunctionArg>,
    outputs: Vec<FunctionArg>,
}

/// A set of functions to be compiled into a shared library.
#[derive(Debug, Clone, Default)]
pub struct SharedLibFunctions {
    functions: Vec<ParsedFunction>,
}

impl SharedLibFunctions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a function to the set of functions to be compiled.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the function.
    /// * `fn_body` - The code of the function.
    /// * `scope` - The set of variables in the current envrionment. This is
    ///   necessary to be able to determine the types and detect code with undefined
    ///   variables.
    pub fn add_fn(
        &mut self,
        name: &str,
        fn_body: &str,
        scope: &[FunctionArg],
    ) -> Result<(), Error> {
        // TODO: restrict the set of inputs.
        // For now, we take the whole scope as input
        let inputs = scope.to_owned();
        let outputs = find_outputs(fn_body, scope)?;
        self.functions.push(ParsedFunction {
            name: name.to_string(),
            fn_body: fn_body.to_string(),
            inputs,
            outputs,
        });
        Ok(())
    }

    /// Generate the code that can be compiled into a shared library
    pub fn code(&self) -> String {
        let mut code = String::new();
        for ParsedFunction {
            fn_body,
            name,
            inputs,
            outputs,
        } in &self.functions
        {
            let inputs = inputs
                .iter()
                .map(|FunctionArg { arg_name, arg_type }| format!("{}: {}", arg_name, arg_type))
                .collect::<Vec<_>>()
                .join(", ");
            let outputs_types = outputs
                .iter()
                .map(|FunctionArg { arg_type, .. }| format!("{arg_type},"))
                .collect::<Vec<_>>()
                .join(" ");
            let outputs_vars = outputs
                .iter()
                .map(|FunctionArg { arg_name, .. }| format!("{arg_name},"))
                .collect::<Vec<_>>()
                .join(" ");
            code.push_str(&format!(
                r#"
#[nomangle]
pub extern "C" fn {name}({inputs}) -> ({outputs_types}) {{
    {fn_body};
    ({outputs_vars})
}}
"#
            ))
        }
        code
    }
}

impl ContextState {
    /// Put variables in scope and mark them as "old" variables
    fn load_scope(&mut self, scope: &[FunctionArg]) {
        for FunctionArg { arg_name, arg_type } in scope {
            self.variable_states.insert(
                arg_name.to_string(),
                VariableState {
                    type_name: arg_type.to_string(),
                    is_mut: false,
                    move_state: VariableMoveState::Available,
                    definition_span: None,
                },
            );
        }
        self.stored_variable_states = self.variable_states.clone();
    }
}
/// Find the variables created during some code execution.
///
/// Those will be the outputs of the code when we turn it into a function.
fn find_outputs(code: &str, scope: &[FunctionArg]) -> Result<Vec<FunctionArg>, Error> {
    let (code_block, code_info) = CodeBlock::from_original_user_code(code);
    // Create empty state
    let config = tmp_config()?;
    let mut state = ContextState::new(config.clone());
    // Add scope variables to state
    state.load_scope(scope);
    // Apply the code to the state
    let code_block = state.apply(code_block, &code_info.nodes)?;

    // Write a dummy Cargo.toml file for rust analyzer
    let module = Module::new()?;
    module.write_cargo_toml(&state)?;
    module.write_config_toml(&state)?;

    // Now run rust analyzer to fix the variable types
    let analysis_code = state.analysis_code(code_block);
    let mut analyzer = RustAnalyzer::new(&config.tmpdir)?;
    if let Err(errors) = analyzer.fix_variable_types(&mut state, analysis_code) {
        return Err(errors);
    }

    // Finally, new variables in state => they are our outputs
    let outputs = state
        .variable_states
        .into_iter()
        .filter(|(_, state)| state.move_state == VariableMoveState::New)
        .map(|(name, state)| FunctionArg {
            arg_name: name,
            arg_type: state.type_name,
        })
        .collect();
    Ok(outputs)
}

/// Create a config from a new temporary directory
fn tmp_config() -> Result<Config, Error> {
    let tmpdir = tempfile::tempdir()?;
    let mut tmpdir_path = PathBuf::from(tmpdir.path());

    if !tmpdir_path.is_absolute() {
        tmpdir_path = std::env::current_dir()?.join(tmpdir_path);
    }

    // Currently not used path (as no child process is spawned)
    let current_exe = PathBuf::new();

    let mut initial_config = create_initial_config(tmpdir_path, current_exe)?;
    // We currently do not handle stdio, so there is no point in trying to
    // display expressions
    initial_config.display_final_expression = false;
    Ok(initial_config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_outputs() {
        let code = r#"
        let b = 2;
        a + b
        "#;
        let scope = vec![FunctionArg {
            arg_name: "a".to_string(),
            arg_type: "i32".to_string(),
        }];
        let outputs = find_outputs(code, &scope).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].arg_name, "b");
        assert_eq!(outputs[0].arg_type, "i32");
    }

    #[test]
    fn test_add_fn() {
        let scope = vec![FunctionArg {
            arg_name: "a".to_string(),
            arg_type: "i32".to_string(),
        }];
        let mut shared_lib = SharedLibFunctions::new();
        shared_lib
            .add_fn("add", "let b = 2;\n a + b", &scope)
            .unwrap();
        let code = shared_lib.code();
        assert!(code
            .split('\n')
            .any(|line| line == "pub extern \"C\" fn add(a: i32) -> (i32,) {"));
    }
}
