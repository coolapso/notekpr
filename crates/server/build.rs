fn main() {
    // `sqlx::migrate!` embeds the migrations; rebuild when one is added (it
    // only notices changes to the files it already knows).
    println!("cargo:rerun-if-changed=migrations");
}
