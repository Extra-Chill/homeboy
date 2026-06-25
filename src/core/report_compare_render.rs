use crate::core::markdown::escape_markdown_table_cell;
use crate::core::report_compare::{NamedCountDelta, ReportCompareReport};

pub(crate) fn render_markdown(report: &ReportCompareReport) -> String {
    let mut out = String::new();
    out.push_str("# Report Compare\n\n");
    out.push_str(&format!("- **Old:** `{}`\n", report.old.source));
    out.push_str(&format!("- **New:** `{}`\n", report.new.source));
    out.push_str(&format!(
        "- **Total findings:** {} -> {} ({})\n",
        report.total.old,
        report.total.new,
        format_delta(report.total.delta)
    ));
    out.push_str(&format!(
        "- **Stable identities:** resolved {}, new {}, persistent {}\n",
        report.identities.resolved, report.identities.introduced, report.identities.persistent
    ));
    render_delta_table(&mut out, "Groups", &report.groups);
    render_delta_table(&mut out, "Kinds", &report.kinds);
    render_delta_table(&mut out, "Fixtures", &report.fixtures);
    out
}

fn render_delta_table(out: &mut String, title: &str, rows: &[NamedCountDelta]) {
    if rows.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {title}\n\n"));
    out.push_str("| Name | Old | New | Delta |\n");
    out.push_str("|---|---:|---:|---:|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            escape_markdown_table_cell(&row.name),
            row.old,
            row.new,
            format_delta(row.delta)
        ));
    }
}

fn format_delta(delta: isize) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}
