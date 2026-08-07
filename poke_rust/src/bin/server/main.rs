//! HTTP API server for the PokeRust web frontend. Loads the dexes once at
//! startup and exposes battle sessions over JSON — see routes.rs for endpoints.
//! Run from `poke_rust/` so the default dex paths resolve.

mod analysis;
mod bot;
mod dto;
mod mapping;
mod routes;
mod session;
mod tracker;
mod tracker_effects;
mod tracker_parse;
mod tracker_render;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, post, put};
use clap::Parser;
use tower_http::cors::CorsLayer;

use routes::AppState;
use session::{Dexes, MetaDexes};

#[derive(Parser, Debug)]
#[command(about = "PokeRust web API server")]
struct Args {
    /// Path to the showdown pokedex data
    #[arg(long, default_value = "../pokemon_info/showdownDex.txt")]
    poke_dex: String,

    /// Path to the showdown move data
    #[arg(long, default_value = "../pokemon_info/showdownMoves.txt")]
    move_dex: String,

    /// Path to the showdown ability data (used by the inference engine for
    /// ability absence/priority reasoning under non-Perfect information modes)
    #[arg(long, default_value = "../pokemon_info/showdownAbilities.txt")]
    ability_dex: String,

    /// Path to the showdown learnset data (used by the inference engine for
    /// Illusion narrowing under non-Perfect information modes)
    #[arg(long, default_value = "../pokemon_info/showdownLearnsets.txt")]
    learnset_dex: String,

    /// Port to bind on 127.0.0.1
    #[arg(long, default_value_t = 3001)]
    port: u16,

    /// Directory for the ignored sprite cache. The server creates it when necessary.
    #[arg(long, default_value = "../sprite_cache")]
    sprite_cache_dir: PathBuf,

    /// Root of the cached Champions usage-stats scrape (see
    /// `meta_scraper/README.md`), used by the Meta Team Generator
    /// (`CreateBattleRequest.p1TeamMode`/`p2TeamMode == "meta"`). Missing or
    /// unloadable is non-fatal — logged at startup, and only a request that
    /// actually asks for a meta team fails (422), not the whole server.
    #[arg(long, default_value = "../meta_scraper/data")]
    meta_dir: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Keep the engine silent. Each request supplies its battle options.
    let _ = poke_rust::VERBOSITY.set(0);

    let dexes = Arc::new(Dexes {
        pokemon_dex: poke_rust::state::dex_data::parse_pokemon_dex(&args.poke_dex),
        move_dex: poke_rust::state::dex_data::parse_move_dex(&args.move_dex),
        ability_dex: poke_rust::state::dex_data::parse_ability_dex(&args.ability_dex),
        learnset_dex: poke_rust::state::dex_data::parse_learnset_dex(&args.learnset_dex),
    });
    println!(
        "Loaded {} Pokemon, {} moves, {} abilities, {} learnsets",
        dexes.pokemon_dex.len(),
        dexes.move_dex.len(),
        dexes.ability_dex.len(),
        dexes.learnset_dex.len()
    );

    std::fs::create_dir_all(&args.sprite_cache_dir)
        .expect("failed to create sprite cache directory");
    let sprite_cache_dir = args
        .sprite_cache_dir
        .canonicalize()
        .expect("failed to resolve sprite cache directory");

    let meta = Arc::new(load_meta_dexes(&args.meta_dir));

    let state = AppState {
        dexes,
        meta,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        tracker_sessions: Arc::new(Mutex::new(HashMap::new())),
        sprite_cache_dir,
        http: reqwest::Client::new(),
        benchmark_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let app = Router::new()
        .route("/api/battles", post(routes::create_battle))
        .route(
            "/api/battles/{id}",
            get(routes::get_battle).delete(routes::delete_battle),
        )
        .route("/api/battles/{id}/commands", get(routes::get_commands))
        .route("/api/battles/{id}/turn", post(routes::submit_turn))
        .route("/api/battles/{id}/analysis", get(routes::get_analysis))
        .route("/api/tracker", post(tracker::create_tracker))
        .route(
            "/api/tracker/{id}",
            get(tracker::get_tracker).delete(tracker::delete_tracker),
        )
        .route("/api/tracker/{id}/events", post(tracker::submit_tracker_events))
        .route("/api/tracker/{id}/preview", post(tracker::preview_tracker_events))
        .route("/api/tracker/{id}/history", put(tracker::rebuild_tracker_history))
        .route("/api/tracker/{id}/completions", get(tracker::get_tracker_completions))
        .route("/api/dex/species", get(routes::get_species_list))
        .route("/api/benchmark", get(routes::run_benchmark))
        .route("/api/sprites", get(routes::get_sprite))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{}", args.port);
    println!("PokeRust server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind port");
    axum::serve(listener, app).await.expect("server error");
}

/// Load both formats' usage-stats caches for the Meta Team Generator.
///
/// Best-effort: `meta_scraper/data` is gitignored and regenerable (see its
/// README), so a fresh clone or an environment that never ran the scraper is
/// expected to be missing it entirely. Logging and continuing with `None`
/// keeps that the server's problem to report per-request (422 on an actual
/// `"meta"` team-mode request), not a reason to refuse to start.
fn load_meta_dexes(root: &std::path::Path) -> MetaDexes {
    let mut dexes = MetaDexes::default();
    for (format, slot) in [
        (
            poke_rust::meta::MetaFormat::Singles,
            &mut dexes.singles,
        ),
        (
            poke_rust::meta::MetaFormat::Doubles,
            &mut dexes.doubles,
        ),
    ] {
        match poke_rust::meta::MetaDex::load(root, None, format) {
            Ok(dex) => {
                println!(
                    "Loaded {} meta {:?} species (season {})",
                    dex.len(),
                    format,
                    dex.season()
                );
                *slot = Some(dex);
            }
            Err(e) => {
                println!(
                    "Meta Team Generator: no usage data for {format:?} ({e}); \
                     meta team requests for this format will 422"
                );
            }
        }
    }
    dexes
}
