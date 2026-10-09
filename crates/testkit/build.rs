// Rebuild when a migration changes: `TestDb` embeds them via `sqlx::migrate!`.
fn main() {
    println!("cargo:rerun-if-changed=../db/migrations");
}
