pub mod apply;
pub mod backup;
pub mod cluster;
pub mod delete;
pub mod describe;
pub mod doctor;
pub mod exec;
pub mod get;
pub mod logs;
pub mod node;
pub mod runtime;

use crate::cli::*;

/// Dispatch a CLI command to the appropriate handler.
pub async fn dispatch(cli: &Cli, client: &reqwest::Client) -> anyhow::Result<()> {
    let base = cli.server.trim_end_matches('/');

    match &cli.command {
        Commands::Cluster { action } => cluster::handle(client, base, action).await,
        Commands::Node { action } => node::handle(client, base, action).await,
        Commands::Get {
            resource,
            namespace,
        } => get::handle(client, base, resource, namespace).await,
        Commands::Describe {
            resource,
            name,
            namespace,
        } => describe::handle(client, base, resource, name, namespace).await,
        Commands::Apply { file, namespace } => apply::handle(client, base, file, namespace).await,
        Commands::Delete {
            resource,
            id,
            file,
            namespace,
        } => {
            delete::handle(
                client,
                base,
                resource.as_deref(),
                id.as_deref(),
                file.as_deref(),
                namespace,
            )
            .await
        }
        Commands::Logs {
            pod_id,
            namespace,
            follow,
        } => logs::handle(client, base, pod_id, namespace, *follow).await,
        Commands::Exec {
            pod_id,
            command,
            namespace,
            interactive,
        } => {
            exec::handle(
                &cli.server,
                &cli.token,
                pod_id,
                command,
                namespace,
                *interactive,
            )
            .await
        }
        Commands::Doctor { fix } => doctor::handle(client, base, *fix).await,
        Commands::Runtime { action } => runtime::handle(client, &cli.server, action).await,
        Commands::Backup { action } => backup::handle_backup(client, base, action).await,
        Commands::Restore {
            from,
            dry_run,
            force,
        } => backup::handle_restore(client, base, from, *dry_run, *force).await,
        Commands::Pm { .. } => unreachable!("handled before dispatch"),
    }
}
