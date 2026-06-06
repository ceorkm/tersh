use std::sync::Arc;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

mod agent_detect;
mod brain;
mod commands;
mod errors;
mod local_terminal;
mod sessions;
mod sftp;
mod ssh;
mod transfers;
mod tunnels;
mod vault;

use brain::BrainRegistry;
pub use errors::AppError;
use local_terminal::LocalTerminalRegistry;
use sessions::SessionRegistry;
use transfers::TransferRegistry;
use tunnels::TunnelRegistry;
use vault::Vault;

pub struct AppState {
    pub sessions: Arc<SessionRegistry>,
    pub local_terminals: Arc<LocalTerminalRegistry>,
    pub vault: Arc<Mutex<Vault>>,
    pub tunnels: Arc<TunnelRegistry>,
    pub transfers: Arc<TransferRegistry>,
    pub brain: Arc<BrainRegistry>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "tersh=info".into()))
        .init();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _guard = runtime.enter();
    let context = tauri::generate_context!();

    let vault = match runtime.block_on(async { Vault::open_default().await }) {
        Ok(vault) => vault,
        Err(err) => {
            tracing::error!("vault open failed: {err}");
            run_startup_error(context, err.to_string());
            return;
        }
    };

    let brain = Arc::new(BrainRegistry::new());
    if let Err(err) = runtime.block_on(brain.restore_from_disk()) {
        tracing::warn!("brain restore_from_disk failed: {err}");
    }

    let state = AppState {
        sessions: Arc::new(SessionRegistry::new()),
        local_terminals: Arc::new(LocalTerminalRegistry::new()),
        vault: Arc::new(Mutex::new(vault)),
        tunnels: Arc::new(TunnelRegistry::new()),
        transfers: Arc::new(TransferRegistry::new()),
        brain,
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::list_hosts,
            commands::add_host,
            commands::delete_host,
            commands::connect,
            commands::start_local_terminal,
            commands::bind_terminal_output,
            commands::disconnect,
            commands::send_input,
            commands::send_input_raw,
            commands::copy_local_image_to_clipboard,
            commands::resize_pty,
            commands::sftp_upload_local,
            commands::sftp_upload_folder_local,
            commands::detect_remote_agent,
            commands::detect_remote_os,
            commands::list_keys,
            commands::delete_key,
            commands::generate_key,
            commands::import_key,
            commands::list_snippets,
            commands::add_snippet,
            commands::update_snippet,
            commands::delete_snippet,
            commands::run_snippet,
            commands::list_known_hosts,
            commands::list_tunnels,
            commands::add_tunnel,
            commands::delete_tunnel,
            commands::start_tunnel,
            commands::stop_tunnel,
            commands::active_tunnels,
            commands::list_session_logs,
            commands::pick_file,
            commands::pick_files,
            commands::pick_folder,
            commands::pick_uploads,
            commands::pick_upload_folder,
            commands::pick_uploads_any,
            commands::sftp_list,
            commands::list_local_dir,
            commands::sftp_download,
            commands::sftp_upload_to,
            commands::sftp_upload_folder_to,
            commands::sftp_cancel_transfer,
            commands::sftp_rename,
            commands::sftp_mkdir,
            commands::sftp_remove,
            commands::sftp_chmod,
            commands::sftp_preview_file,
            commands::save_file_dialog,
            commands::default_download_path,
            commands::reveal_in_finder,
            commands::open_external_url,
            commands::diag_log,
            commands::update_host,
            commands::export_vault,
            commands::import_vault,
            commands::export_vault_to_file,
            commands::import_vault_from_file,
            commands::list_active_keypass_keys,
            commands::set_key_passphrase,
            commands::clear_key_passphrase,
            commands::set_host_password,
            commands::clear_host_password,
            commands::has_host_password,
            commands::prompt_enhance,
            commands::prompt_enhancer_get_api_key,
            commands::prompt_enhancer_set_api_key,
            commands::brain_enable_local,
            commands::brain_enable_remote,
            commands::brain_disable,
            commands::brain_refresh,
            commands::brain_reconnect_resync,
            commands::brain_list,
            commands::brain_hydrate_remote,
            commands::brain_list_remote_projects,
        ])
        .run(context)
        .unwrap_or_else(|err| tracing::error!("error while running tersh: {err}"));
}

fn run_startup_error(context: tauri::Context, error: String) {
    let message = format!(
        "Tersh could not open your encrypted vault.\n\n{error}\n\nYour vault was not replaced or overwritten."
    );
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let handle = app.handle().clone();
            app.dialog()
                .message(message.clone())
                .title("Vault Open Failed")
                .kind(MessageDialogKind::Error)
                .buttons(MessageDialogButtons::Ok)
                .show(move |_| {
                    handle.exit(1);
                });
            Ok(())
        })
        .run(context)
        .unwrap_or_else(|err| tracing::error!("error while showing vault startup failure: {err}"));
}
