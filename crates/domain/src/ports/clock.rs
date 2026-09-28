use crate::entities::ObservedAt;

pub trait Clock: Send + Sync {
    fn now(&self) -> ObservedAt;
}
