use anyhow::Result;
use colored::Colorize;
use indoc::formatdoc;
use std::collections::BTreeMap;

const FIRST_COLUMN_MIN_WIDTH: usize = 10;
const MIN_BOX_WIDTH: usize = 20;
const MAX_BOX_WIDTH: usize = 80;

pub struct Table {
    name: String,
    rows: BTreeMap<String, String>,
}

impl Table {
    pub fn new(name: String, rows: BTreeMap<String, String>) -> Self {
        Self { name, rows }
    }
    pub fn get_string(&self) -> Result<String> {
        let title_str = format!(" Variables for {} ", self.name);
        let title_width = console::measure_text_width(title_str.as_str());

        let max_right_content = self
            .rows
            .iter()
            .flat_map(|(_, content)| {
                content
                    .split('\n')
                    .map(console::measure_text_width)
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0);
        let max_right_content = std::cmp::max(max_right_content, title_width);
        let first_column_width = std::cmp::max(
            FIRST_COLUMN_MIN_WIDTH,
            self.rows
                .keys()
                .map(|name| console::measure_text_width(name))
                .max()
                .unwrap_or(0),
        );

        let edge = format!("{} ", box_drawing::double::VERTICAL);
        let edge_width = console::measure_text_width(edge.as_str());

        let middle_padding = format!(" {} ", box_drawing::light::VERTICAL);
        let middle_padding_width = console::measure_text_width(middle_padding.as_str());
        let middle_padding = middle_padding.cyan().dimmed().to_string();

        let box_width =
            ((edge_width * 2) + first_column_width + middle_padding_width + max_right_content)
                .clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH);

        let second_column_width =
            box_width - (edge_width * 2) - first_column_width - middle_padding_width;

        let title_side_padding = ((box_width as f64) - (title_width as f64) - 2.0) / 2.0;

        let top_box = format!(
            "{}{}{}{}{}",
            box_drawing::double::DOWN_RIGHT.cyan().dimmed(),
            str::repeat(
                box_drawing::double::HORIZONTAL,
                title_side_padding.ceil() as usize
            )
            .cyan()
            .dimmed(),
            title_str.magenta().bold(),
            str::repeat(
                box_drawing::double::HORIZONTAL,
                title_side_padding.floor() as usize
            )
            .cyan()
            .dimmed(),
            box_drawing::double::DOWN_LEFT.cyan().dimmed(),
        );

        let bottom_box = format!(
            "{}{}{}",
            box_drawing::double::UP_RIGHT.cyan().dimmed(),
            str::repeat(box_drawing::double::HORIZONTAL, box_width - 2)
                .cyan()
                .dimmed(),
            box_drawing::double::UP_LEFT.cyan().dimmed()
        );

        let hor_sep = format!(
            "{}{}{}",
            box_drawing::double::VERTICAL.cyan().dimmed(),
            str::repeat(box_drawing::light::HORIZONTAL, box_width - 2)
                .cyan()
                .dimmed(),
            box_drawing::double::VERTICAL.cyan().dimmed()
        );

        let phase_rows = self
            .rows
            .clone()
            .into_iter()
            .map(|(name, content)| {
                print_row(
                    name.as_str(),
                    content.as_str(),
                    edge.as_str(),
                    middle_padding.as_str(),
                    first_column_width,
                    second_column_width,
                    false,
                )
            })
            .collect::<Vec<_>>()
            .join(format!("\n{hor_sep}\n").as_str());

        Ok(formatdoc! {"
          {}
          {}
          {}
          ",
          top_box,
          phase_rows,
          bottom_box
        })
    }
    pub fn print(&self) -> Result<()> {
        println!("{}", self.get_string()?);
        Ok(())
    }
}

fn print_row(
    title: &str,
    content: &str,
    left_edge: &str,
    middle: &str,
    first_column_width: usize,
    second_column_width: usize,
    indent_second_line: bool,
) -> String {
    let mut textwrap_opts = textwrap::Options::new(second_column_width);
    textwrap_opts.break_words = true;
    if indent_second_line {
        textwrap_opts.subsequent_indent = " ";
    }

    let right_edge = left_edge.chars().rev().collect::<String>();

    let list_lines = textwrap::wrap(content, textwrap_opts);
    let mut output = format!(
        "{}{}{}{}{}",
        left_edge.cyan().dimmed(),
        console::pad_str(title, first_column_width, console::Alignment::Left, None).bold(),
        middle,
        console::pad_str(
            &list_lines[0],
            second_column_width,
            console::Alignment::Left,
            None
        ),
        right_edge.cyan().dimmed()
    );

    for line in list_lines.iter().skip(1) {
        output = format!(
            "{}\n{}{}{}{}{}",
            output,
            left_edge.cyan().dimmed(),
            console::pad_str("", first_column_width, console::Alignment::Left, None),
            middle,
            console::pad_str(line, second_column_width, console::Alignment::Left, None),
            right_edge.cyan().dimmed()
        );
    }

    output
}
