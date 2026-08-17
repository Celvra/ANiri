fn main() {
    println!("cargo:rustc-check-cfg=cfg(have_libinput_plugin_system)");
    if pkg_config::Config::new()
        .atleast_version("1.30.0")
        .probe("libinput")
        .is_ok()
    {
        println!("cargo:rustc-cfg=have_libinput_plugin_system")
    }

    // The anland audio bridge links against libpipewire.
    let pipewire = pkg_config::Config::new().probe("libpipewire-0.3");

    let mut build = cc::Build::new();
    build
        .file("src/backend/anland/c/libdisplay_producer/display_producer.c")
        .file("src/backend/anland/c/common/socket_utils.c")
        .include("src/backend/anland/c")
        .warnings(false);

    if let Ok(lib) = &pipewire {
        for path in &lib.include_paths {
            build.include(path);
        }
        for lib in &lib.libs {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
        for path in &lib.link_paths {
            println!("cargo:rustc-link-search=native={}", path.display());
        }
        println!("cargo:rustc-cfg=have_anland_audio");
        build.file("src/backend/anland/c/anland_audio.c");
        build.file("src/backend/anland/c/anland_camera.c");
    }

    build.compile("display_producer");
}
