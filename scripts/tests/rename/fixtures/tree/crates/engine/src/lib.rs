//! Kept: ORM_TEST_DATABASE_URL names the test database `orm_test`.
use orm_core::ir;

pub fn url() -> Option<String> {
    std::env::var("ORM_DATABASE_URL").ok()
}
