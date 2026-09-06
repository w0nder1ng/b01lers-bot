use serenity::all::{EditChannel, EditMessage, PermissionOverwriteType};

use crate::commands::competition::{get_competition_from_ctx, get_join_message, join_button};
use crate::commands::{has_perms, CmdContext, Error};
use crate::config::config;

/// Archives the current competition channel.
#[poise::command(slash_command)]
pub async fn archive(ctx: CmdContext<'_>) -> Result<(), Error> {
    // category where archived ctf channels are sent
    let archived_category_id = config().server.archived_ctf_category_id;
    let join_channel_id = config().server.ctf_join_channel;
    let join_channel = join_channel_id.to_channel(ctx).await?;

    if !has_perms(&ctx).await {
        return Err(anyhow::anyhow!(
            "You do not have permissions to archive a competition."
        ));
    }

    // Ensure command is being run within a competition channel, and the competition is not already archived.
    let competition = get_competition_from_ctx(&ctx).await?;

    let mut channel = competition
        .channel_id
        .to_channel(ctx)
        .await?
        .guild()
        .expect("You are not inside a competition channel.");

    if channel
        .parent_id
        .is_some_and(|id| id == archived_category_id)
    {
        return Err(anyhow::anyhow!("This competition is already archived!"));
    }

    // Remove the channel from active CTFS
    let mut conn = ctx.data().conn().await;
    conn.remove_active_ctf(competition.channel_id).await?;
    conn.commit().await?;

    // Edit button to no longer provide access
    let active = ctx.data().conn().await.get_active_ctfs().await?;
    let edit = active.iter().fold(EditMessage::new(), |edit, ctf| {
        edit.button(join_button(&ctf.name, ctf.channel_id))
    });
    let mut join_message = get_join_message(&ctx, &join_channel.guild().unwrap()).await?;
    join_message.edit(&ctx, edit).await?;

    // Remove viewing restrictions
    let roles = &ctx
        .guild()
        .ok_or(anyhow::anyhow!("Failed to get roles"))?
        .roles
        .clone();
    let everyone = roles
        .values()
        .find(|role| role.name == "@everyone")
        .ok_or(anyhow::anyhow!("\\@everyone role not found"))?;
    channel
        .delete_permission(ctx, PermissionOverwriteType::Role(everyone.id))
        .await?;

    // Move the channel to the archived category.
    channel
        .edit(ctx, EditChannel::new().category(archived_category_id))
        .await?;

    ctx.say(format!("Archived **{}**.", competition.name))
        .await?;

    Ok(())
}
