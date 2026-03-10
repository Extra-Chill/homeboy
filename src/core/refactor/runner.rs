use crate::component::Component;
use crate::extension::{self, ExtensionCapability, ExtensionExecutionContext};

pub fn resolve_lint_command(component: &Component) -> crate::Result<ExtensionExecutionContext> {
    extension::resolve_execution_context(component, ExtensionCapability::Lint)
}

pub fn resolve_test_command(component: &Component) -> crate::Result<ExtensionExecutionContext> {
    extension::resolve_execution_context(component, ExtensionCapability::Test)
}
