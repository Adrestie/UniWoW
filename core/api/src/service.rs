use std::marker::PhantomData;

/// Names a service and the type it is provided as. Providers and consumers use the same constant,
/// declared next to the service's interface, so they cannot disagree on the type.
pub struct ServiceKey<T> {
    id: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> ServiceKey<T> {
    pub const fn new(id: &'static str) -> Self {
        Self { id, _type: PhantomData }
    }

    /// The id written in `requires` and `uses` of `[package.metadata.uniwow]`.
    pub const fn id(&self) -> &'static str {
        self.id
    }
}

impl<T> Clone for ServiceKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ServiceKey<T> {}
