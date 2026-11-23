use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "These commands are supported:"
)]
pub enum Command {
    #[command(description = "Start a new bot")]
    Start,
    #[command(description = "Show list of all commands")]
    Help,
    #[command(description = "Get statistics of memes")]
    Stats,
    #[command(description = "Show meme random meme")]
    Random,
    #[command(description = "Set up your stuff")]
    Settings,
    #[command(description = "Swipe and rate memes (Meme Tinder)")]
    Tinder,
    #[command(description = "Browse your liked memes")]
    Liked,
    #[command(description = "Random meme subscription (Meme Drip)")]
    Drip,
    #[command(description = "Browse meme categories and tags")]
    Tags,
}
