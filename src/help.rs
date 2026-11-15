use crate::terminal::line::Line;
use crate::terminal::parse::parse_command_output;

const HELP: &str = "\
Keys

  h              Show this help
  q              Quit
  r              Run the command now
  j, Down, Enter Scroll down
  k, Up          Scroll up
  Space, PgDn    Page down
  PgUp           Page up
  g              Top
  G              Bottom
  /              Search forward from the top of the screen
  ?              Search backward from the bottom of the screen
  n              Repeat search
  N              Repeat search in reverse
  -i             Toggle ignore case
  -I             Always ignore case
  ^l             Redraw

q or h closes this page.
In the prompt, Enter searches. Esc cancels it or clears Pattern not found.
";

pub(crate) fn help_lines() -> Vec<Line> {
    parse_command_output(HELP.as_bytes())
}

#[cfg(test)]
mod tests {
    use crate::height::Height;
    use crate::help::help_lines;
    use crate::render::Renderer;
    use crate::size::Size;
    use crate::terminal::width::Width;
    use crate::terminal_bg::TerminalBg;

    #[test]
    fn test_help_lists_keys() {
        let text: String = help_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref().as_str())
                    .collect::<String>()
            })
            .collect::<Vec<String>>()
            .join("\n");
        assert_eq!(true, text.contains("h              Show this help"));
        assert_eq!(true, text.contains("q              Quit"));
        assert_eq!(true, text.contains("q or h closes this page."));
        assert_eq!(
            true,
            text.contains("/              Search forward from the top of the screen")
        );
        assert_eq!(
            true,
            text.contains("?              Search backward from the bottom of the screen")
        );
        assert_eq!(true, text.contains("n              Repeat search"));
    }

    #[test]
    fn test_help_draws_key_list() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = Size {
            width: Width(80),
            height: Height(24),
        };
        renderer.set_lines(help_lines(), size);
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&buf);
        assert_eq!(true, parser.screen().contents().contains("Show this help"));
        assert_eq!(true, parser.screen().contents().contains("(END)"));
    }
}
