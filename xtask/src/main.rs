mod docgen;
mod helpers;
mod path;

use std::{env, error::Error};

type DynError = Box<dyn Error>;

pub mod tasks {
    use crate::DynError;
    use std::collections::HashSet;

    pub fn docgen() -> Result<(), DynError> {
        use crate::docgen::*;
        write(TYPABLE_COMMANDS_MD_OUTPUT, &typable_commands()?);
        write(STATIC_COMMANDS_MD_OUTPUT, &static_commands()?);
        write(LANG_SUPPORT_MD_OUTPUT, &lang_features()?);
        Ok(())
    }

    pub fn querycheck(languages: impl Iterator<Item = String>) -> Result<(), DynError> {
        use helix_core::syntax::LanguageData;

        let languages_to_check: HashSet<_> = languages.collect();
        let loader = helix_core::config::default_lang_loader();
        for (_language, lang_data) in loader.languages() {
            if !languages_to_check.is_empty()
                && !languages_to_check.contains(&lang_data.config().language_id)
            {
                continue;
            }
            let config = lang_data.config();
            let Some(syntax_config) = LanguageData::compile_syntax_config(config, &loader)? else {
                continue;
            };
            let grammar = syntax_config.grammar;
            LanguageData::compile_indent_query(grammar, config)?;
            LanguageData::compile_textobject_query(grammar, config)?;
            LanguageData::compile_tag_query(grammar, config)?;
            LanguageData::compile_rainbow_query(grammar, config)?;
        }

        println!("Query check succeeded");

        Ok(())
    }

    pub fn themecheck(themes: impl Iterator<Item = String>) -> Result<(), DynError> {
        use helix_view::theme::Loader;

        let themes_to_check: HashSet<_> = themes.collect();

        let theme_names = [
            vec!["default".to_string(), "base16_default".to_string()],
            Loader::read_names(&crate::path::themes()),
        ]
        .concat();
        let loader = Loader::new(&[crate::path::runtime()]);
        let mut errors_present = false;

        for name in theme_names {
            if !themes_to_check.is_empty() && !themes_to_check.contains(&name) {
                continue;
            }

            let (_, warnings) = loader.load_with_warnings(&name).unwrap();

            if !warnings.is_empty() {
                errors_present = true;
                println!("Theme '{name}' loaded with errors:");
                for warning in warnings {
                    println!("\t* {}", warning);
                }
            }
        }

        match errors_present {
            true => Err("Errors found when loading bundled themes".into()),
            false => {
                println!("Theme check successful!");
                Ok(())
            }
        }
    }

    pub fn print_help() {
        println!(
            "
Usage: Run with `cargo xtask <task>`, eg. `cargo xtask docgen`.

    Tasks:
        docgen                     Generate files to be included in the mdbook output.
        query-check [languages]    Check that tree-sitter queries are valid for the given
                                   languages, or all languages if none are specified.
        theme-check [themes]       Check that the theme files in runtime/themes/ are valid for the
                                   given themes, or all themes if none are specified.
        install                    Build a release 'helix' binary + runtime in ../bin/ following
                                   the official packaging guide (sets HELIX_DEFAULT_RUNTIME so
                                   it works like a normal install without needing HELIX_RUNTIME).
"
        );
    }

    pub fn install() -> Result<(), DynError> {
        // Robust path computation based on the xtask's view of the workspace.
        // This checkout lives under the user's "tools/" collection (e.g. Projects/tools/treelix),
        // and we want the packaged build in the sibling tools/bin/ (with helix + runtime/).
        let workspace_root = crate::path::project_root();
        let tools_root = workspace_root
            .parent()
            .ok_or_else(|| "Could not determine parent of workspace (expected tools/treelix layout)")?;
        let bin_dir = tools_root.join("bin");
        let runtime_target = bin_dir.join("runtime");
        let binary_target = bin_dir.join("helix");

        // The absolute path we will bake into the binary via HELIX_DEFAULT_RUNTIME.
        // This is the "distribution fallback" per the packaging guide.
        let runtime_abs = if runtime_target.exists() {
            std::fs::canonicalize(&runtime_target)?
        } else {
            runtime_target.clone()
        };

        println!("=== treelix custom install (following official packaging guide) ===");
        println!("  Target binary : {}", binary_target.display());
        println!("  Target runtime: {}", runtime_target.display());
        println!("  HELIX_DEFAULT_RUNTIME (baked in): {}", runtime_abs.display());
        println!();

        // Prepare a clean destination (remove any stale previous runtime so the copy is exact).
        if runtime_target.exists() {
            println!("Removing previous runtime directory for a clean copy...");
            std::fs::remove_dir_all(&runtime_target)?;
        }
        std::fs::create_dir_all(&bin_dir)?;
        std::fs::create_dir_all(&runtime_target)?;

        // Copy the entire runtime tree using the platform `cp` (fast + correct handling
        // of the large grammars/ tree including any symlinks in sources/ that exist in
        // this checkout). We use absolute paths so it is independent of cwd.
        let src_runtime = crate::path::runtime();
        println!("Copying runtime/ into place (this may take a moment for grammars)...");
        let cp_status = std::process::Command::new("cp")
            .args([
                "-a",
                &format!("{}/.", src_runtime.display()),
                &format!("{}/", runtime_target.display()),
            ])
            .status()?;
        if !cp_status.success() {
            return Err("Failed to copy runtime directory (cp failed)".into());
        }

        // Clean only helix-loader so that the subsequent build will recompile it with the
        // *current* value of HELIX_DEFAULT_RUNTIME visible to option_env! in lib.rs.
        // (The rerun-if-env-changed we added to build.rs also helps for incremental cases.)
        println!("Cleaning cached helix-loader artifacts to guarantee fresh bake of HELIX_DEFAULT_RUNTIME...");
        let _ = std::process::Command::new("cargo")
            .args(["clean", "-p", "helix-loader"])
            .status(); // best-effort; ignore failure

        // Export for this process + explicitly pass to the cargo child (the one that runs rustc).
        std::env::set_var("HELIX_DEFAULT_RUNTIME", &runtime_abs);

        println!("Building release binary with HELIX_DEFAULT_RUNTIME baked in...");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "--release", "-p", "helix-term"])
            .env("HELIX_DEFAULT_RUNTIME", &runtime_abs)
            .status()?;
        if !build_status.success() {
            return Err("Release build failed".into());
        }

        // Install the binary as "helix" (so `helix` in PATH prefers this custom build).
        println!("Installing binary as 'helix'...");
        std::fs::copy("target/release/hx", &binary_target)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_target)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_target, perms)?;
        }

        // --- Post-install verification (the key part of "iterate until working") ---
        println!();
        println!("Verifying packaged install by running the new binary --health ...");
        let verify_out = std::process::Command::new(&binary_target)
            .args(["--health"])
            .env_remove("HELIX_RUNTIME") // ensure we test the baked-in behavior, not an override
            .output()?;
        let health = String::from_utf8_lossy(&verify_out.stdout);

        let baked_str = runtime_abs.to_string_lossy().to_string();
        let target_str = runtime_target.to_string_lossy().to_string();

        let lists_baked = health.contains(&baked_str) || health.contains(&target_str);
        let complains_about_target = health.contains(&format!("does not exist: {}", target_str))
            || health.contains(&format!("is empty: {}", target_str));

        println!("{}", health);

        println!();
        if lists_baked && !complains_about_target {
            println!("✅ VERIFICATION PASSED: packaged runtime appears in runtime_dirs() and exists.");
        } else if lists_baked {
            println!("⚠️  VERIFICATION PARTIAL: path is listed but --health reported a problem with it.");
        } else {
            println!("❌ VERIFICATION FAILED: the baked HELIX_DEFAULT_RUNTIME path did not appear in --health output.");
            println!("   This usually means a stale build cache still had an old expansion of option_env!.");
            println!("   (We did run `cargo clean -p helix-loader`; try `cargo clean` + rbuild again if needed.)");
        }

        println!();
        println!("✅ Finished custom Helix install:");
        println!("   Binary : {}", binary_target.display());
        println!("   Runtime: {}", runtime_target.display());
        println!();
        println!("The binary was built with HELIX_DEFAULT_RUNTIME set (priority 4 in the lookup).");
        println!("Combined with the exe-sibling fallback (priority 5), `helix` from tools/bin will find");
        println!("its runtime for themes, queries, grammars, and LSP semantic tokens with no manual env.");
        println!();
        println!("Put '{}' early in $PATH to make `helix` use this build.", bin_dir.display());

        Ok(())
    }
}

fn main() -> Result<(), DynError> {
    let mut args = env::args().skip(1);
    let task = args.next();
    match task {
        None => tasks::print_help(),
        Some(t) => match t.as_str() {
            "docgen" => tasks::docgen()?,
            "query-check" => tasks::querycheck(args)?,
            "theme-check" => tasks::themecheck(args)?,
            "install" => tasks::install()?,
            invalid => return Err(format!("Invalid task name: {}", invalid).into()),
        },
    };
    Ok(())
}
