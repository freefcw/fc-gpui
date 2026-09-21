#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]
#![cfg_attr(not(target_os = "windows"), allow(unused))]

use std::env;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(gles)");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(feature = "windows-manifest")]
        embed_resource();

        #[cfg(target_os = "windows")]
        windows::build();
    }
}

#[cfg(feature = "windows-manifest")]
fn embed_resource() {
    let resource_dir = std::path::Path::new("resources/windows");
    let manifest = resource_dir.join("gpui.manifest.xml");
    let rc_file = resource_dir.join("gpui.rc");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", rc_file.display());
    embed_resource::compile(rc_file, embed_resource::ParamsIncludeDirs([resource_dir]))
        .manifest_required()
        .unwrap();
}

#[cfg(target_os = "windows")]
mod windows {
    use std::{
        fs,
        io::Write,
        path::{Path, PathBuf},
        process::{self, Command},
    };

    pub(super) fn build() {
        #[cfg(not(debug_assertions))]
        compile_shaders();
    }

    fn compile_shaders() {
        let shader_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("src/windows/shaders.hlsl");
        let out_dir = std::env::var("OUT_DIR").unwrap();

        println!("cargo:rerun-if-changed={}", shader_path.display());
        let fxc_path = find_fxc_compiler();
        let modules = [
            "quad",
            "shadow",
            "path_rasterization",
            "path_sprite",
            "underline",
            "monochrome_sprite",
            "polychrome_sprite",
        ];

        let rust_binding_path = format!("{out_dir}/shaders_bytes.rs");
        if Path::new(&rust_binding_path).exists() {
            fs::remove_file(&rust_binding_path)
                .expect("Failed to remove existing Rust binding file");
        }
        for module in modules {
            compile_shader_for_module(
                module,
                &out_dir,
                &fxc_path,
                shader_path.to_str().unwrap(),
                &rust_binding_path,
            );
        }

        let shader_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("src/windows/color_text_raster.hlsl");
        compile_shader_for_module(
            "emoji_rasterization",
            &out_dir,
            &fxc_path,
            shader_path.to_str().unwrap(),
            &rust_binding_path,
        );
    }

    fn find_fxc_compiler() -> String {
        if let Ok(path) = std::env::var("GPUI_FXC_PATH")
            && Path::new(&path).exists()
        {
            return path;
        }

        if let Ok(output) = std::process::Command::new("where.exe")
            .arg("fxc.exe")
            .output()
            && output.status.success()
        {
            let path = String::from_utf8_lossy(&output.stdout);
            return path.trim().to_string();
        }

        if Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe")
            .exists()
        {
            return r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe"
                .to_string();
        }

        panic!("Failed to find fxc.exe");
    }

    fn compile_shader_for_module(
        module: &str,
        out_dir: &str,
        fxc_path: &str,
        shader_path: &str,
        rust_binding_path: &str,
    ) {
        let output_file = format!("{out_dir}/{module}_vs.h");
        let const_name = format!("{}_VERTEX_BYTES", module.to_uppercase());
        compile_shader_impl(
            fxc_path,
            &format!("{module}_vertex"),
            &output_file,
            &const_name,
            shader_path,
            "vs_4_1",
        );
        generate_rust_binding(&const_name, &output_file, rust_binding_path);

        let output_file = format!("{out_dir}/{module}_ps.h");
        let const_name = format!("{}_FRAGMENT_BYTES", module.to_uppercase());
        compile_shader_impl(
            fxc_path,
            &format!("{module}_fragment"),
            &output_file,
            &const_name,
            shader_path,
            "ps_4_1",
        );
        generate_rust_binding(&const_name, &output_file, rust_binding_path);
    }

    fn compile_shader_impl(
        fxc_path: &str,
        entry_point: &str,
        output_path: &str,
        var_name: &str,
        shader_path: &str,
        target: &str,
    ) {
        let output = Command::new(fxc_path)
            .args([
                "/T",
                target,
                "/E",
                entry_point,
                "/Fh",
                output_path,
                "/Vn",
                var_name,
                "/O3",
                shader_path,
            ])
            .output();

        match output {
            Ok(result) => {
                if result.status.success() {
                    return;
                }
                eprintln!(
                    "Shader compilation failed for {}:\n{}",
                    entry_point,
                    String::from_utf8_lossy(&result.stderr)
                );
                process::exit(1);
            }
            Err(error) => {
                eprintln!("Failed to run fxc for {entry_point}: {error}");
                process::exit(1);
            }
        }
    }

    fn generate_rust_binding(const_name: &str, head_file: &str, output_path: &str) {
        let header_content = fs::read_to_string(head_file).expect("Failed to read header file");
        let const_definition = {
            let global_var_start = header_content.find("const BYTE").unwrap();
            let global_var = &header_content[global_var_start..];
            let equal = global_var.find('=').unwrap();
            global_var[equal + 1..].trim()
        };
        let rust_binding = format!(
            "const {}: &[u8] = &{}\n",
            const_name,
            const_definition.replace('{', "[").replace('}', "]")
        );
        let mut options = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_path)
            .expect("Failed to open Rust binding file");
        options
            .write_all(rust_binding.as_bytes())
            .expect("Failed to write Rust binding file");
    }
}
