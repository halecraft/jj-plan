/// Print the `jj plan --help` text and return.
///
/// This matches the zsh shim's help output, extended with new commands
/// that will be implemented in subsequent plans.
pub fn print_help() {
    print!(
        "\
jj plan — plan-oriented programming commands

Subcommands:
  stack [name] [-r REV]    Start a new named stack (creates change + bookmark)
  new [flags] [jj-new-args]
                           Create a plan change with a self-referencing placeholder
    --first                Insert before the first stack member (moves bookmark)
    --last                 Insert after the last stack member
  config                   Show resolved configuration and stack info

Options:
  --help, -h               Show this help message
"
    );
}