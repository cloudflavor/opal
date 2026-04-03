use crate::{Cli, CompletionArgs};
use anyhow::Result;
use std::io;
use structopt::StructOpt;

pub(crate) fn execute(args: CompletionArgs) -> Result<()> {
    let mut app = Cli::clap();
    app.gen_completions_to("opal", args.shell.to_clap_shell(), &mut io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_completion_mentions_opal_command() {
        let mut app = Cli::clap();
        let mut output = Vec::new();
        app.gen_completions_to(
            "opal",
            crate::CompletionShell::Bash.to_clap_shell(),
            &mut output,
        );
        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("opal"));
        assert!(text.contains("complete -F"));
    }
}
