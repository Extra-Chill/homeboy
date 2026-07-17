pub use homeboy_product_identity::BuildIdentity;

pub fn current() -> BuildIdentity {
    homeboy_product_identity::build_identity()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_package_version_does_not_override_product_build_version() {
        assert_eq!(env!("CARGO_PKG_VERSION"), "0.1.0");
        assert_eq!(
            current().version,
            homeboy_product_identity::product_version()
        );
        assert_ne!(current().version, env!("CARGO_PKG_VERSION"));
    }
}
