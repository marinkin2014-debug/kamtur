use chrono::Utc;
use domain::entities::ObservedAt;
use domain::ports::Clock;

pub struct SystemClock;

impl Clock for SystemClock {
    #[inline]
    fn now(&self) -> ObservedAt {
        Utc::now()
    }
}
