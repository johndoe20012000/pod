// MCP schema guard: every argument a tool handler reads must be declared in that tool's input schema.
//
// An MCP client only knows a tool by the `inputSchema` Pod publishes in `tools/list`, and that schema is built from
// the `ArgSpec`s handed to `McpTool::with_args`. A handler that reads `args["types"]` without a matching `ArgSpec`
// publishes a schema that never mentions `types`, so a client that builds its call from the schema cannot send it and
// the call can only fail. This scan catches that drift at `cargo test` time.
//
// Run it in isolation with:
//
//   cargo test --test mcp_schema_args -- --nocapture
//
// Like `tests/i18n_parity.rs` this is a standalone integration-test crate that reads the sources off disk, because
// the `pod` binary crate exposes no lib to link against.
//
// WHAT IT SCANS (`src/services/mcp/*.rs`, test modules excluded):
//   * tools        — every top-level fn that builds a tool with `McpTool::new("name", ...)`.
//   * declarations — the first string literal of each `ArgSpec::<constructor>("key", ...)` in the tool fn, plus those
//                    of the argument-less helpers it calls (`dry_run_arg()`, `paginated_character_args()`).
//   * reads        — `args.get("key")` and `<helper>(&args, "key")` in the tool fn, plus those of every helper the
//                    arguments are handed to (`parse_plan_tree(&args)`, `merged_watch(&existing, &args)`).
//
// Helpers are followed transitively by name, same file first, then any other file of the module. Only reads through a
// binding named `args` count, which is the module's convention for the raw tool arguments: fields read off an array
// element (`item.get("type_id")`) belong to the nested item schema, not to the top-level arguments.

use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  path::{Path, PathBuf},
};

const TOOL_FILES: [&str; 6] = [
  "tools_captains_log.rs",
  "tools_dossier.rs",
  "tools_mail.rs",
  "tools_objectives.rs",
  "tools_read.rs",
  "tools_write.rs",
];

struct Function {
  body: String,
  file: String,
  name: String,
}

struct Tool {
  declared: BTreeSet<String>,
  file: String,
  name: String,
  read: BTreeSet<String>,
}

fn mcp_dir() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("src/services/mcp")
}

fn is_ident_char(c: char) -> bool {
  c.is_ascii_alphanumeric() || c == '_'
}

// The identifier `text` ends with, unless it is a method name (`tool.with_args`): methods are not module helpers.
fn trailing_function_name(text: &str) -> Option<&str> {
  let start = text.rfind(|c: char| !is_ident_char(c)).map_or(0, |index| index + 1);
  let name = &text[start..];
  (!name.is_empty() && !text[..start].ends_with('.')).then_some(name)
}

// Drops whitespace except a single space between two identifiers, so a call rustfmt wrapped over several lines
// (`args\n    .get("types")`) reads the same as one it kept on a single line while `match args` stays two words.
fn compact(source: &str) -> String {
  let mut out = String::with_capacity(source.len());
  let mut pending_space = false;
  for c in source.chars() {
    if c.is_whitespace() {
      pending_space = true;
      continue;
    }
    if pending_space && is_ident_char(c) && out.chars().next_back().is_some_and(is_ident_char) {
      out.push(' ');
    }
    pending_space = false;
    out.push(c);
  }
  out
}

fn production_source(source: &str) -> &str {
  source
    .find("\n#[cfg(test)]\nmod tests")
    .map_or(source, |index| &source[..index])
}

fn function_name(line: &str) -> Option<&str> {
  let mut rest = line;
  if let Some(after) = rest.strip_prefix("pub") {
    rest = after;
    if rest.starts_with('(') {
      rest = &rest[rest.find(')')? + 1..];
    }
    rest = rest.trim_start();
  }
  rest = rest.strip_prefix("async ").unwrap_or(rest);
  let rest = rest.strip_prefix("fn ")?;
  let end = rest.find(|c: char| !is_ident_char(c))?;
  Some(&rest[..end])
}

// rustfmt puts a top-level fn at column zero and closes it with a line that is exactly `}`.
fn top_level_functions(file: &str, source: &str) -> Vec<Function> {
  let mut functions = Vec::new();
  let mut open: Option<(&str, String)> = None;
  for line in production_source(source).lines() {
    if open.is_none() {
      open = function_name(line).map(|name| (name, String::new()));
    }
    let Some((name, text)) = open.as_mut() else {
      continue;
    };
    text.push_str(line);
    text.push('\n');
    if line == "}" {
      functions.push(Function {
        body: compact(text),
        file: file.to_owned(),
        name: (*name).to_owned(),
      });
      open = None;
    }
  }
  functions
}

fn literals_after(body: &str, marker: &str) -> BTreeSet<String> {
  let mut found = BTreeSet::new();
  for (index, _) in body.match_indices(marker) {
    if body[..index].chars().next_back().is_some_and(is_ident_char) {
      continue;
    }
    let rest = &body[index + marker.len()..];
    if let Some(end) = rest.find('"') {
      found.insert(rest[..end].to_owned());
    }
  }
  found
}

fn call_len(call: &str) -> usize {
  let mut depth = 0usize;
  for (index, c) in call.char_indices() {
    match c {
      '(' => depth += 1,
      ')' if depth <= 1 => return index + 1,
      ')' => depth -= 1,
      _ => {}
    }
  }
  call.len()
}

// The item fields nested in an `object_array(...)` are not top-level arguments, so the scan resumes after the call.
fn declared_keys(body: &str) -> BTreeSet<String> {
  let mut keys = BTreeSet::new();
  let mut rest = body;
  while let Some(index) = rest.find("ArgSpec::") {
    rest = &rest[index + "ArgSpec::".len()..];
    let constructor_end = rest.find(|c: char| !is_ident_char(c)).unwrap_or(rest.len());
    let (constructor, call) = rest.split_at(constructor_end);
    let Some(literal) = call.strip_prefix("(\"") else {
      continue;
    };
    if let Some(end) = literal.find('"') {
      keys.insert(literal[..end].to_owned());
    }
    if constructor.ends_with("object_array") {
      rest = &call[call_len(call)..];
    }
  }
  keys
}

fn read_keys(body: &str) -> BTreeSet<String> {
  let mut keys = literals_after(body, "args.get(\"");
  keys.extend(literals_after(body, "args,\""));
  keys
}

fn argumentless_calls(body: &str) -> BTreeSet<String> {
  body
    .match_indices("()")
    .filter_map(|(index, _)| trailing_function_name(&body[..index]))
    .map(str::to_owned)
    .collect()
}

// Free functions that take the raw `args` in any position: walks back from each `args` argument to the `(` that
// opens its call.
fn calls_taking_args(body: &str) -> BTreeSet<String> {
  let mut names = BTreeSet::new();
  for (index, _) in body.match_indices("args") {
    let before = body[..index].strip_suffix('&').unwrap_or(&body[..index]);
    let after = &body[index + "args".len()..];
    if before.chars().next_back().is_some_and(is_ident_char) || !after.starts_with([',', ')']) {
      continue;
    }
    let mut depth = 0usize;
    for (position, c) in before.char_indices().rev() {
      match c {
        ')' => depth += 1,
        '(' if depth > 0 => depth -= 1,
        '(' => {
          names.extend(trailing_function_name(&before[..position]).map(str::to_owned));
          break;
        }
        _ => {}
      }
    }
  }
  names
}

fn tool_name(body: &str) -> Option<String> {
  literals_after(body, "McpTool::new(\"").into_iter().next()
}

fn load_functions() -> Vec<Function> {
  let mut paths: Vec<PathBuf> = fs::read_dir(mcp_dir())
    .expect("the mcp module directory exists")
    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
    .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
    .collect();
  paths.sort();
  let mut functions = Vec::new();
  for path in paths {
    let file = path
      .file_name()
      .and_then(|name| name.to_str())
      .expect("a utf-8 file name");
    let source = fs::read_to_string(&path).expect("a readable source file");
    functions.extend(top_level_functions(file, &source));
  }
  functions
}

fn scan() -> Vec<Tool> {
  let functions = load_functions();
  let mut by_name: BTreeMap<&str, Vec<&Function>> = BTreeMap::new();
  for function in &functions {
    by_name.entry(function.name.as_str()).or_default().push(function);
  }
  let resolve = |name: &str, from: &str| -> Option<&Function> {
    let candidates = by_name.get(name)?;
    candidates
      .iter()
      .find(|candidate| candidate.file == from)
      .or_else(|| candidates.first())
      .copied()
  };
  let closure = |root: &Function, callees: fn(&str) -> BTreeSet<String>, keys: fn(&str) -> BTreeSet<String>| {
    let mut found = BTreeSet::new();
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(next) = pending.pop() {
      if !seen.insert((next.file.as_str(), next.name.as_str())) {
        continue;
      }
      found.extend(keys(&next.body));
      pending.extend(
        callees(&next.body)
          .iter()
          .filter_map(|callee| resolve(callee, &next.file)),
      );
    }
    found
  };

  functions
    .iter()
    .filter_map(|function| {
      let name = tool_name(&function.body)?;
      Some(Tool {
        declared: closure(function, argumentless_calls, declared_keys),
        file: function.file.clone(),
        name,
        read: closure(function, calls_taking_args, read_keys),
      })
    })
    .collect()
}

#[test]
fn every_argument_a_handler_reads_is_declared_in_the_tool_schema() {
  let mut undeclared = Vec::new();
  for tool in scan() {
    for key in tool.read.difference(&tool.declared) {
      undeclared.push(format!(
        "  {}: `{}` reads `{key}` but declares no ArgSpec for it",
        tool.file, tool.name
      ));
    }
  }

  assert!(
    undeclared.is_empty(),
    "{} argument(s) are read by a handler but missing from the tool's published input schema, so a client that \
    follows the schema can never send them:\n{}",
    undeclared.len(),
    undeclared.join("\n")
  );
}

#[test]
fn the_scan_follows_tools_into_their_helpers() {
  let tools = scan();

  let files: BTreeSet<&str> = tools.iter().map(|tool| tool.file.as_str()).collect();
  for file in TOOL_FILES {
    assert!(files.contains(file), "the scan found no tool in {file}");
  }

  let list_mail = tools
    .iter()
    .find(|tool| tool.name == "list_mail")
    .expect("the scan finds list_mail");
  for key in ["character_id", "limit", "page"] {
    assert!(list_mail.read.contains(key), "list_mail reads `{key}`");
    assert!(list_mail.declared.contains(key), "list_mail declares `{key}`");
  }

  let planner_create = tools
    .iter()
    .find(|tool| tool.name == "planner_create")
    .expect("the scan finds planner_create");
  for key in ["name", "product_type_id", "runs", "types"] {
    assert!(planner_create.read.contains(key), "planner_create reads `{key}`");
  }
}
