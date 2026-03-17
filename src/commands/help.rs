/// Print the `jj plan --help` text and return.
///
/// Lists all available plan subcommands and their flags.
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
  done [flags] [CHANGE_ID]
                           Mark a plan as done, strip [scratch] sections, advance
    --stack                Mark all plans in the stack as done
    --keep-scratch         Keep [scratch] sections (don't strip)
    --dry-run              Show what would be stripped without modifying anything
  next                     Advance @ to the next plan in the stack
  prev                     Move @ to the previous plan in the stack
  go <N | CHANGE_ID>       Jump to a plan by index (1-based) or change ID
  config                   Show resolved configuration and stack info

Options:
  --help, -h               Show this help message
"
    );
}