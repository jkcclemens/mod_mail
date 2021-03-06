use crate::{
  database::model::config::Config,
  error::*,
};

use chrono::{DateTime, Duration, Utc};

use diesel::{
  pg::PgConnection,
  prelude::*,
};

use parking_lot::RwLock;

use rand::{Rng, thread_rng, distributions::Alphanumeric};

use r2d2::PooledConnection;

use r2d2_diesel::ConnectionManager;

use serenity::{
  http::AttachmentType,
  prelude::*,
  model::{
    channel::{ChannelType, Message, PermissionOverwrite, PermissionOverwriteType},
    event::ResumedEvent,
    gateway::{Activity, Ready},
    guild::Guild,
    id::{GuildId, UserId},
    permissions::Permissions,
    user::OnlineStatus,
  },
};

use std::collections::{BTreeMap, HashMap};

type Connection = PooledConnection<ConnectionManager<PgConnection>>;

pub struct Listener {
  pool: r2d2::Pool<ConnectionManager<PgConnection>>,
  states: RwLock<HashMap<UserId, RwLock<State>>>,
}

impl Listener {
  pub fn new(pool: r2d2::Pool<ConnectionManager<PgConnection>>) -> Self {
    Listener {
      pool,
      states: Default::default(),
    }
  }
}

impl Listener {
  fn update_presence(&self, ctx: &Context) {
    ctx.set_presence(
      Some(Activity::playing("DM to contact the mods.")),
      OnlineStatus::Online,
    );
  }
}

impl EventHandler for Listener {
  fn ready(&self, ctx: Context, _: Ready) {
    self.update_presence(&ctx);
  }

  fn resume(&self, ctx: Context, _: ResumedEvent) {
    self.update_presence(&ctx);
  }

  fn message(&self, ctx: Context, message: Message) {
    if !self.states.read().contains_key(&message.author.id) {
      self.states.write().insert(message.author.id, State::new(&message));
    }
    let states = self.states.read();
    let state = &states[&message.author.id];
    let mut state = state.write();
    let conn = match self.pool.get() {
      Ok(p) => p,
      Err(e) => {
        eprintln!("could not get connection from pool: {:#?}", e);
        return;
      },
    };
    if let Err(e) = state.process(&ctx, &message, &conn) {
      eprintln!("error in processing: {:#?}", e);
    }
  }

  fn guild_create(&self, ctx: Context, guild: Guild, new: bool) {
    if new {
      return;
    }

    ctx.shard.chunk_guilds(vec![guild.id], None, None);
  }
}

#[derive(Debug)]
struct State {
  last_message: DateTime<Utc>,
  stage: Stage,
}

impl State {
  fn new(message: &Message) -> RwLock<Self> {
    RwLock::new(State {
      last_message: message.timestamp.with_timezone(&Utc),
      stage: Default::default(),
    })
  }

  fn process(&mut self, ctx: &Context, message: &Message, conn: &Connection) -> Result<()> {
    if !message.channel(ctx).map(|x| x.private().is_some()).unwrap_or_default() {
      return Ok(());
    }

    if message.author.id == ctx.cache.read().user.id {
      return Ok(());
    }

    if self.last_message + Duration::minutes(30) < Utc::now() {
      self.stage = Stage::Default;
    }
    self.last_message = message.timestamp.with_timezone(&Utc);
    let stage = match self.stage {
      Stage::Default => self.do_default(ctx, message, conn),
      Stage::ChoosingGuild(ref original_message, ref guilds) => self.do_choosing_guild(ctx, message, original_message, guilds),
      Stage::Cooldown(finished) => self.do_cooldown(ctx, message, conn, finished),
    };
    if let Some(stage) = stage? {
      self.stage = stage;
    }
    Ok(())
  }

  fn guilds(&self, ctx: &Context, conn: &Connection, user: UserId) -> Result<BTreeMap<String, Config>> {
    let mut shared_guilds: BTreeMap<GuildId, String> = ctx.cache
      .read()
      .guilds
      .values()
      .filter(|g| g.read().members.keys().any(|&m| m == user))
      .map(|x| (x.read().id, x.read().name.clone()))
      .collect();

    use crate::database::schema::configs;
    let cs: BTreeMap<GuildId, Config> = configs::table
      .filter(
        configs::server_id.eq_any(&shared_guilds.keys().map(|x| x.0 as i64).collect::<Vec<_>>())
        .and(
          configs::moderator_role.is_not_null()
        )
      )
      .load::<Config>(&**conn)
      .chain_err(|| "could not load server configs")?
      .into_iter()
      .map(|c| (GuildId(c.server_id as u64), c))
      .collect();

    let guilds: BTreeMap<String, Config> = cs
      .into_iter()
      .map(|(id, config)| (shared_guilds.remove(&id).unwrap(), config))
      .collect();

    Ok(guilds)
  }

  fn do_cooldown(&self, ctx: &Context, message: &Message, conn: &Connection, finished: DateTime<Utc>) -> Result<Option<Stage>> {
    let now = Utc::now();
    if now < finished {
      let dur = finished.signed_duration_since(now);
      let mins = dur.num_minutes();
      let secs = dur.num_seconds() - (mins * 60);
      let mut time = String::new();
      if mins > 0 {
        time += &format!("{} minute{}", mins, if mins == 1 { "" } else { "s" });
      }
      if secs > 0 {
        if !time.is_empty() {
          time += " and ";
        }
        time += &format!("{} second{}", secs, if secs == 1 { "" } else { "s" });
      }

      let msg = format!(
        "You must wait until {} to use Mod Mail again.",
        time,
      );
      message.channel_id.send_message(ctx, |m| m.content(&msg)).chain_err(|| "could not send message")?;
      return Ok(None);
    }

    self.do_default(ctx, message, conn)
  }

  fn do_default(&self, ctx: &Context, message: &Message, conn: &Connection) -> Result<Option<Stage>> {
    let guilds = self.guilds(ctx, conn, message.author.id)?;
    if guilds.is_empty() {
      return Ok(Some(Stage::Default));
    }

    if guilds.len() == 1 {
      let (name, config) = guilds.iter().next().unwrap();
      self.do_relay(ctx, message, &name, &config)?;
      return Ok(Some(Stage::Cooldown(Utc::now() + Duration::minutes(10))));
    }
    message.channel_id.send_message(ctx, |m| m
      .content("Thanks for sending me a message. Messages sent to me will be relayed to a private channel between you and the Discord server's moderation team.

**Note**: I'll forget all about you contacting me if you become inactive for more than 30 minutes.")
      )
      .chain_err(|| "could not send message")?;
    let names = guilds.keys().map(|x| x.as_str()).collect::<Vec<_>>().join("\n");
    let msg = format!(
      ":thinking: **Choose a server**\n\nYou're in {} servers with me, so I'm not sure which one you want to contact.

Please **send the name of the server as I've listed below** to let me know which one you want to contact.

{}",
      guilds.len(),
      names,
    );
    message.channel_id.send_message(ctx, |m| m.content(msg)).chain_err(|| "could not send message")?;
    Ok(Some(Stage::ChoosingGuild(box message.clone(), box guilds)))
  }

  fn do_choosing_guild(&self, ctx: &Context, message: &Message, original: &Message, guilds: &BTreeMap<String, Config>) -> Result<Option<Stage>> {
    let choice = guilds
      .iter()
      .map(|(name, config,)| (name, config, strsim::normalized_damerau_levenshtein(&message.content.to_lowercase(), &name.to_lowercase())))
      .max_by(|(_, _, a), (_, _, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Less));
    let (name, config) = match choice {
      Some((name, config, score)) if score > 0.75 => (name, config),
      _ => {
        message.channel_id
          .send_message(ctx, |m| m.content("I couldn't tell what you meant. Please send the name of the Discord server exactly as I've listed."))
          .chain_err(|| "could not send message")?;
        return Ok(None);
      },
    };
    self.do_relay(ctx, original, name, config)?;
    Ok(Some(Stage::Cooldown(Utc::now() + Duration::minutes(10))))
  }

  fn do_relay(&self, ctx: &Context, message: &Message, guild_name: &str, config: &Config) -> Result<()> {
      let msg = format!(
        "In a few seconds, I will create a new channel in the **{}** Discord server and relay your message. I will also mention you and the moderators.\n
**Important**: Please continue the conversation in the new channel and not in this DM. Thanks!",
        guild_name,
      );
      message.channel_id.send_message(ctx, |m| m.content(msg)).chain_err(|| "could not send message")?;

      let random: String = std::iter::repeat(())
        .map(|()| thread_rng().sample(Alphanumeric))
        .take(7)
        .collect();

      let guild_id = GuildId(config.server_id as u64);

      let guild = guild_id.to_guild_cached(ctx).chain_err(|| "missing cached guild")?;

      let everyone = guild
        .read()
        .roles
        .iter()
        .find(|(_, role)| role.name == "@everyone")
        .map(|(id, _)| *id)
        .chain_err(|| "no @everyone role")?;

      let (mod_role_id, mod_role) = guild
        .read()
        .roles
        .iter()
        .find(|(_, role)| Some(&role.name) == config.moderator_role.as_ref())
        .map(|(id, role)| (*id, role.clone()))
        .chain_err(|| "cannot find moderator role")?;

      let current_id = ctx.cache.read().user.id;

      let channel = guild_id
        .create_channel(
          ctx,
          |c| {
            c
              .name(format!("mod_mail_{}", random))
              .kind(ChannelType::Text)
              .permissions(vec![
                // don't let anyone read the channel by default
                PermissionOverwrite {
                  kind: PermissionOverwriteType::Role(everyone),
                  allow: Permissions::empty(),
                  deny: Permissions::READ_MESSAGES,
                },
                // let mods use the channel
                PermissionOverwrite {
                  kind: PermissionOverwriteType::Role(mod_role_id),
                  allow: Permissions::READ_MESSAGES | Permissions::SEND_MESSAGES | Permissions::ATTACH_FILES,
                  deny: Permissions::empty(),
                },
                // let the reporter use the channel
                PermissionOverwrite {
                  kind: PermissionOverwriteType::Member(message.author.id),
                  allow: Permissions::READ_MESSAGES | Permissions::SEND_MESSAGES | Permissions::ATTACH_FILES,
                  deny: Permissions::empty(),
                },
                // let the bot use the channel
                PermissionOverwrite {
                  kind: PermissionOverwriteType::Member(current_id),
                  allow: Permissions::READ_MESSAGES | Permissions::SEND_MESSAGES | Permissions::ATTACH_FILES,
                  deny: Permissions::empty(),
                },
              ]);
            if let Some(category) = config.category {
              c.category(category as u64);
            }
            c
          },
        )
        .chain_err(|| "could not create channel")?;

      std::thread::sleep(Duration::seconds(3).to_std().unwrap());

      let mut fix_mentionable = false;
      if !mod_role.mentionable {
        fix_mentionable = true;
        mod_role.edit(ctx, |e| e.mentionable(true)).ok();
      }

      let msg = format!(
        "From: {}\nTo: {}\n\nOriginal message below:",
        message.author.mention(),
        mod_role_id.mention(),
      );
      channel.send_message(ctx, |m| m.content(&msg)).chain_err(|| "could not send message")?;
      if fix_mentionable {
        mod_role.edit(ctx, |e| e.mentionable(false)).ok();
      }
      if message.attachments.is_empty() {
        channel.send_message(ctx, |m| m.content(&message.content)).chain_err(|| "could not send message")?;
      } else {
        // FIXME: maybe don't download and instead just use the proxy_url
        let attachments: Vec<(Vec<u8>, &str)> = message
          .attachments
          .iter()
          .map(|a| a.download().chain_err(|| "could not download attachment").map(|d| (d, a.filename.as_str())))
          .collect::<Result<_>>()?;
        let files: Vec<AttachmentType> = attachments
          .iter()
          .map(|(bs, n)| AttachmentType::Bytes((bs.as_slice(), *n)))
          .collect();
        channel.send_files(ctx, files, |m| m.content(&message.content)).chain_err(|| "could not send message")?;
      }

      channel
        .delete_permission(ctx, PermissionOverwriteType::Member(current_id))
        .chain_err(|| "could not update permissions on channel")?;

      Ok(())
  }
}

#[derive(Debug)]
enum Stage {
  /// The default stage. Assume never contacted before.
  Default,
  /// The user is choosing a guild to send the message to.
  /// The guilds with Mod Mail enabled that the user is in.
  ChoosingGuild(Box<Message>, Box<BTreeMap<String, Config>>),
  /// The user has recently used Mod Mail and must wait until the specified date to do so again.
  Cooldown(DateTime<Utc>),
}

impl Default for Stage {
  fn default() -> Self {
    Stage::Default
  }
}
