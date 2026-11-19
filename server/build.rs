// `sqlx::migrate!` embeds server/migrations at compile time; rebuild when a migration is added or changed.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
