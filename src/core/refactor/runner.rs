use crate::component::Component;
use crate::extension::{self, ExtensionCapability, ResolvedExtensionCommand};

pub fn resolve_lint_command(component: &Component) -> crate::Result<ResolvedExtensionCommand> {
    extension::resolve_extension_command(component, ExtensionCapability::Lint)
}

pub fn resolve_test_command(component: &Component) -> crate::Result<ResolvedExtensionCommand> {
    extension::resolve_extension_command(component, ExtensionCapability::Test)
}
