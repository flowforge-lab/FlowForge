//! Test fixture: exits 1 immediately. Used by the supervisor integration test to
//! verify that a server that never reaches a working state is parked in `Failed`
//! after `max_failures` consecutive connect attempts.
fn main() {
    std::process::exit(1);
}
