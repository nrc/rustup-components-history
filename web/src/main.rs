mod opts;
mod tiers_table;

use chrono::{NaiveDate, Utc};
use either::Either;
use failure::{format_err, ResultExt};
use handlebars::{handlebars_helper, Handlebars};
use opts::Config;
use rustup_available_packages::{
    cache::{FsCache, NoopCache},
    table::Table,
    AvailabilityData, Downloader,
};
use serde::Serialize;
use std::{
    fs::{create_dir_all, File},
    io::{self, Write},
    path::{Path, PathBuf},
};
use structopt::StructOpt;
use tiers_table::TiersTable;

#[derive(StructOpt)]
#[structopt(about = "Rust tools per-release availability monitor")]
enum CmdOpts {
    #[structopt(name = "render", about = "Renders pages using provided configuration")]
    Render(ConfigOpt),
    #[structopt(
        name = "print_config",
        about = "Prints the default configuration to stdout"
    )]
    PrintConfig,
}

#[derive(StructOpt)]
struct ConfigOpt {
    #[structopt(
        short = "c",
        long = "config",
        help = "Path to a configuration file",
        parse(from_os_str)
    )]
    config_path: PathBuf,
}

#[derive(Serialize)]
struct PathRenderData<'a> {
    target: &'a str,
}

fn setup_logger(verbosity: log::LevelFilter) -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}] {}",
                record.target(),
                record.level(),
                message
            ))
        })
        .level(verbosity)
        .chain(io::stderr())
        .apply()?;
    Ok(())
}

#[derive(Serialize)]
struct TiersData<'a> {
    tiers: TiersTable<'a>,
    datetime: String,
}

fn generate_html(
    data: &AvailabilityData,
    dates: &[NaiveDate],
    opts::Html {
        template_path,
        output_pattern,
        tiers,
    }: opts::Html,
) -> Result<(), failure::Error> {
    const TEMPLATE_NAME: &str = "target_info";
    let mut handlebars = Handlebars::new();
    handlebars_helper!(streq: |x: str, y: str| x  == y);
    handlebars.register_helper("streq", Box::new(streq));
    handlebars.set_strict_mode(true);
    handlebars
        .register_template_file(TEMPLATE_NAME, &template_path)
        .with_context(|_| format!("File path: {:?}", &template_path))?;

    let all_targets = data.get_available_targets();

    let additional = TiersData {
        tiers: TiersTable::new(tiers, &all_targets),
        datetime: Utc::now().format("%d %b %Y, %H:%M:%S UTC").to_string(),
    };

    for target in &all_targets {
        log::info!("Processing target {}", target);
        let output_path = handlebars
            .render_template(&output_pattern, &PathRenderData { target })
            .with_context(|_| format!("Invalid output pattern: {}", &output_pattern))?;
        if let Some(parent) = Path::new(&output_path).parent() {
            create_dir_all(parent)
                .with_context(|_| format!("Can't create path {}", parent.display()))?;
        }
        log::info!("Preparing file {}", output_path);
        let out = File::create(&output_path)
            .with_context(|_| format!("Can't create file [{}]", output_path))?;

        let table = Table::builder(&data, target)
            .dates(dates)
            .additional(&additional)
            .build();

        log::info!("Writing target {} to {:?}", target, output_path);
        handlebars
            .render_to_write(TEMPLATE_NAME, &table, out)
            .with_context(|_| format!("Can't render [{:?}] for [{}]", template_path, target))?;
    }
    Ok(())
}

fn generate_fs_tree(data: &AvailabilityData, output: &Path) -> Result<(), failure::Error> {
    let targets = data.get_available_targets();
    let pkgs = data.get_available_packages();

    for target in targets {
        let target_path = output.join(target);
        create_dir_all(&target_path)
            .with_context(|_| format!("Can't create path {}", target_path.display()))?;
        for pkg in &pkgs {
            if let Some(date) = data.last_available(target, pkg) {
                let path = target_path.join(pkg);
                let mut f = File::create(&path)
                    .with_context(|_| format!("Can't create file {}", path.display()))?;
                writeln!(f, "{}", date.format("%Y-%m-%d"))?;
            } else {
                // If a package is not available, don't create a file for it at all.
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), failure::Error> {
    let cmd_opts = CmdOpts::from_args();
    let config = match cmd_opts {
        CmdOpts::Render(cmd_opts) => Config::load(&cmd_opts.config_path)
            .with_context(|_| format!("Can't load config {:?}", cmd_opts.config_path))?,
        CmdOpts::PrintConfig => {
            println!("{}", Config::default_with_comments());
            return Ok(());
        }
    };
    setup_logger(config.verbosity)?;

    let mut data: AvailabilityData = Default::default();
    let cache = if let Some(cache_path) = config.cache_path.as_ref() {
        Either::Left(
            FsCache::new(cache_path).map_err(|e| format_err!("Can't initialize cache: {}", e))?,
        )
    } else {
        Either::Right(NoopCache {})
    };
    let downloader = Downloader::with_default_source(&config.channel)
        .set_cache(cache)
        .skip_missing_days(7);
    let manifests =
        downloader.get_last_manifests(config.days_in_past + config.additional_lookup_days)?;
    let dates: Vec<_> = manifests
        .iter()
        .map(|manifest| manifest.date)
        .take(config.days_in_past)
        .collect();
    data.add_manifests(manifests);
    log::info!("Available targets: {:?}", data.get_available_targets());
    log::info!("Available packages: {:?}", data.get_available_packages());

    generate_html(&data, &dates, config.html)?;
    generate_fs_tree(&data, &config.file_tree_output)?;

    Ok(())
}
