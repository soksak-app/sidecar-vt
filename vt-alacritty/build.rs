fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        pkg_config::Config::new()
            .statik(true)
            .probe("soksak-process-identity")
            .expect(
                "the declared soksak process identity package must be available through pkg-config",
            );
    }
}
