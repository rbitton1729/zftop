use std::process::Command;

#[test]
fn help_documents_pools_tree_controls() {
    let output = Command::new(env!("CARGO_BIN_EXE_zftop"))
        .arg("--help")
        .output()
        .expect("run zftop --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help stdout is utf8");
    let lines: Vec<&str> = stdout.lines().collect();

    let pools_start = lines
        .iter()
        .position(|line| line.trim() == "(Pools tree)")
        .unwrap_or_else(|| panic!("missing Pools tree section: {stdout}"));
    let pools_detail_start = lines
        .iter()
        .position(|line| line.trim() == "(Pools detail)")
        .unwrap_or_else(|| panic!("missing Pools detail section: {stdout}"));
    let datasets_start = lines
        .iter()
        .position(|line| line.trim() == "(Datasets tree)")
        .unwrap_or_else(|| panic!("missing Datasets tree section: {stdout}"));

    assert!(
        pools_start < pools_detail_start && pools_detail_start < datasets_start,
        "Pools tree/detail sections should come before Datasets tree: {stdout}"
    );

    let pools_tree_block = lines[pools_start..pools_detail_start].join("\n");
    assert!(
        pools_tree_block.contains("Select row"),
        "Pools tree help should describe row selection: {pools_tree_block}"
    );
    assert!(
        pools_tree_block.contains("Jump to first / last visible"),
        "Pools tree help should describe visible-row Home/End behavior: {pools_tree_block}"
    );
    assert!(
        pools_tree_block.contains("Expand selected pool"),
        "Pools tree help should document pool expansion: {pools_tree_block}"
    );
    assert!(
        pools_tree_block.contains("Collapse selected pool, or jump to parent pool"),
        "Pools tree help should document collapse / parent-jump behavior: {pools_tree_block}"
    );

    let pools_detail_block = lines[pools_detail_start..datasets_start].join("\n");
    assert!(
        pools_detail_block.contains("Return to tree"),
        "Pools detail help should describe returning to the tree: {pools_detail_block}"
    );
    assert!(
        !stdout.contains("(Pools list)"),
        "stale list wording should be gone: {stdout}"
    );
    assert!(
        !stdout.contains("Select pool"),
        "stale pool-only selection wording should be gone: {stdout}"
    );
}
