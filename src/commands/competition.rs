use anyhow::Context;
use serenity::all::Message;
use serenity::all::{
    Builder, ChannelFlags, ChannelId, ChannelType, CreateButton, CreateChannel, CreateEmbed,
    CreateForumTag, CreateMessage, EditChannel, EditMessage, EditThread, ForumEmoji,
    PermissionOverwrite, PermissionOverwriteType, Permissions, ReactionType,
};
use serenity::builder::CreateForumPost;
use serenity::model::channel::GuildChannel;

use crate::config::config;
use crate::db::{BingoSquare, Challenge, Competition, DbConn};

use super::{has_perms, CmdContext, Error};

pub fn join_button(name: &str, id: ChannelId) -> CreateButton {
    CreateButton::new(format!("{}", id)).label(format!("Play in {}", name))
}

pub async fn get_join_message(
    ctx: &CmdContext<'_>,
    join_channel: &GuildChannel,
) -> Result<Message, Error> {
    let existing_message = match join_channel.last_message_id {
        Some(id) => join_channel.message(ctx, id).await.ok(),
        None => None,
    };

    // If the message doesn't exist or can't be accessed (e.g. if it was deleted),
    // publish a new message. either way, store as join_message
    Ok(match existing_message {
        Some(msg) => msg,
        None => {
            join_channel
                .send_message(ctx, CreateMessage::new().content("Join an active CTF:"))
                .await?
        }
    })
}

pub async fn update_join_message(
    ctx: &CmdContext<'_>,
    conn: &mut DbConn,
    join_channel: &GuildChannel,
    new_channel: &GuildChannel,
    new_name: &str,
) -> Result<(), Error> {
    let active = conn.get_active_ctfs().await?;

    // Update the join message with a button for each active CTF/division pair
    let edit = active.iter().fold(EditMessage::new(), |edit, ctf| {
        edit.button(join_button(&ctf.name, ctf.channel_id))
    });

    let mut join_message = get_join_message(ctx, join_channel).await?;
    // Include a button for the new CTF
    join_message
        .edit(ctx, edit.button(join_button(new_name, new_channel.id)))
        .await?;

    Ok(())
}

const CATEGORIES: &[(&str, &str)] = &[
    ("welcome", "🎉"),
    ("web", "🌐"),
    ("crypto", "🧮"),
    ("pwn", "💥"),
    ("rev", "🛠️"),
    ("misc", "⚙️"),
    ("forensics", "🔍"),
    ("osint", "🕵️"),
    ("blockchain", "⛓"),
    ("programming", "👨‍💻"),
    ("jail", "🚔"),
    ("unsolved", "❌"),
    ("solved", "✅"),
];

struct CtfDetails {
    name: String,
    url: String,
    username: String,
    password: String,
}
impl CtfDetails {
    fn new(name: String, url: String, username: String, password: String) -> Result<Self, Error> {
        if username.contains("`") {
            anyhow::bail!("Username cannot include a backtick (\\`)")
        }

        if password.contains("`") {
            anyhow::bail!("Password cannot include a backtick (\\`)")
        }

        Ok(Self {
            name,
            url,
            username,
            password,
        })
    }
}
async fn create_forum_post(
    ctx: &CmdContext<'_>,
    details: &CtfDetails,
) -> Result<GuildChannel, Error> {
    let server = &config().server;
    let (everyone_id, officers_id) = {
        // something about async lifetimes not working?? so this goes in a block
        let guild = ctx.guild().context("couldn't get guild")?;
        let roles = &guild.roles;
        (
            roles
                .values()
                .find(|role| role.name == "@everyone")
                .context("\\@everyone role not found")?
                .id,
            roles
                .values()
                .find(|role| role.name == server.officer_role)
                .ok_or(anyhow::anyhow!("officer role not found"))?
                .id,
        )
    };

    let CtfDetails {
        name,
        url,
        username,
        password,
    } = details;

    let username_esc = format!("`{username}`");
    let password_esc = format!("`{password}`");
    // TODO: prettier error
    // Create forum channel
    let creds_str =
        &format!("**{name}**\n{url}\n\n**Username**: {username_esc}\n**Password**: {password_esc}");
    let mut forum = CreateChannel::new(name.to_string())
        .category(server.ctf_category_id)
        .position(1)
        .kind(ChannelType::Forum)
        .default_reaction_emoji(ForumEmoji::Id(server.ctf_default_emoji_id))
        .topic(creds_str) // Post guidelines for forum channel
        // deny access to everyone except officers by default
        .permissions([
            PermissionOverwrite {
                kind: PermissionOverwriteType::Role(everyone_id),
                allow: Permissions::empty(),
                deny: Permissions::VIEW_CHANNEL,
            },
            PermissionOverwrite {
                kind: PermissionOverwriteType::Role(officers_id),
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::empty(),
            },
        ])
        .execute(ctx, server.guild_id)
        .await?;

    // Add category and solved tags to forum channel
    let tags = CATEGORIES.iter().map(|(name, emoji)| {
        CreateForumTag::new(name.to_string()).emoji(ReactionType::Unicode(emoji.to_string()))
    });
    forum
        .edit(ctx, EditChannel::new().available_tags(tags))
        .await?;

    // Create post with credentials
    let credentials_embed = CreateEmbed::new()
        .color(0xc22026)
        .title(&format!("{name} credentials"))
        .description(url)
        .field("Username", username_esc, false)
        .field("Password", password_esc, false);

    let mut creds_channel = forum
        .create_forum_post(
            ctx,
            CreateForumPost::new(
                "Credentials + general discussion",
                CreateMessage::new().add_embed(credentials_embed),
            ),
        )
        .await?;

    // Pin credentials / general discussion post
    creds_channel
        .edit_thread(ctx, EditThread::new().flags(ChannelFlags::PINNED))
        .await?;

    // Pin credentials message in creds channel
    if let Some(creds_message_id) = creds_channel.last_message_id {
        let creds_message = creds_channel.message(ctx, creds_message_id).await?;
        creds_message.pin(ctx).await?;
    }

    Ok(forum)
}

/// Creates a new ctf competition channel.
#[poise::command(slash_command)]
pub async fn competition(
    ctx: CmdContext<'_>,
    #[description = "Name of the ctf"] name: String,
    #[description = "Url of ctf website"] url: String,
    //#[description = "Description of the ctf"] description: Option<String>,
    #[description = "Team username"] username: String,
    #[description = "Team password or login url"] password: String,
) -> Result<(), Error> {
    let server = &config().server;
    let ctf_category_id = server.ctf_category_id;
    let channels = ctx.guild().context("Failed to get guild")?.channels.clone();

    if channels
        .keys()
        .filter(|id| channels[id].parent_id == Some(ctf_category_id))
        .any(|ctf| channels[ctf].name == name)
    {
        anyhow::bail!("CTF channel already exists")
    }

    // Defer response because channel setup may take longer than 3 seconds
    ctx.defer().await?;

    if !has_perms(&ctx).await {
        anyhow::bail!("You do not have permissions to create a competition.")
    }

    let join_channel = &channels[&server.ctf_join_channel];
    let mut conn = ctx.data().conn().await;

    let details = CtfDetails::new(name, url, username, password)?;
    let forum = create_forum_post(&ctx, &details).await?;
    update_join_message(&ctx, &mut conn, join_channel, &forum, &details.name).await?;

    let competition = Competition {
        channel_id: forum.id,
        name: details.name.clone(),
        bingo: BingoSquare::Free.into(),
        active: true,
    };
    conn.create_competition(competition).await?;

    conn.commit().await?;

    ctx.say(format!(
        "Created channel for **{}**: {forum}",
        &details.name
    ))
    .await?;

    Ok(())
}

pub async fn get_competition_id_from_ctx(ctx: &CmdContext<'_>) -> Result<ChannelId, Error> {
    let Some(thread_channel) = ctx.guild_channel().await else {
        Err(anyhow::anyhow!("You are not inside a competition channel."))?
    };

    // For a forum channel, the competition channel will be the command channel's parent.
    let Some(channel_id) = thread_channel.parent_id else {
        Err(anyhow::anyhow!("You are not inside a competition channel."))?
    };

    Ok(channel_id)
}

/// Gets the competition in the channel the command was invoked from.
pub async fn get_competition_from_ctx(ctx: &CmdContext<'_>) -> Result<Competition, Error> {
    let channel_id = get_competition_id_from_ctx(ctx).await?;

    let competition = ctx
        .data()
        .conn()
        .await
        .get_competition(channel_id)
        .await
        .with_context(|| "You are not inside a competition channel.")?;

    Ok(competition)
}

pub async fn get_challenge_from_ctx(ctx: &CmdContext<'_>) -> Result<Challenge, Error> {
    let Some(thread_channel) = ctx.guild_channel().await else {
        Err(anyhow::anyhow!("You are not inside a challenge channel."))?
    };

    let challenge = ctx
        .data()
        .conn()
        .await
        .get_challenge_by_channel_id(thread_channel.id)
        .await
        .with_context(|| "You are not inside a challenge channel.")?;

    Ok(challenge)
}
