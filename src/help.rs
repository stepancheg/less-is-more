use crate::output::line::Line;
use crate::output::parse::parse_command_output;

const HELP: &str = "\
Keys

  h              Show this help
  q              Quit
  r              Run the command now
  j, Down        Scroll down
  k, Up          Scroll up
  Space, PgDn    Page down
  PgUp           Page up
  g              Top
  G              Bottom
  Ctrl-L         Redraw

q or h closes this page.
";

pub(crate) fn help_lines() -> Vec<Line> {
    parse_command_output(HELP.as_bytes())
}

#[cfg(test)]
mod tests {
    use crate::height::Height;
    use crate::help::help_lines;
    use crate::output::width::Width;
    use crate::render::Renderer;
    use crate::size::Size;
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
    }

    #[test]
    fn test_help_draws_key_list() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = Size {
            width: Width(40),
            height: Height(24),
        };
        renderer.set_lines(help_lines(), size);
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt100::Parser::new(24, 40, 0);
        parser.process(&buf);
        assert_eq!(true, parser.screen().contents().contains("Show this help"));
        assert_eq!(true, parser.screen().contents().contains("(END)"));
    }
}
