pub use homeboy_product_identity::BuildIdentity;

pub fn current() -> BuildIdentity {
    homeboy_product_identity::build_identity()
}
