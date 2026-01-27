use ada_url::Url;
use chrono::{DateTime, FixedOffset, TimeDelta};
use regex::RegexBuilder;
use std::{
    collections::{BTreeMap, btree_map::Entry},
    env, fs,
    io::{self, BufRead, Write},
    ops::Sub,
};

const OUTPUT_TEMPLATE: &str = include_str!("../template/index.html");

fn main() {
    let find_sync = RegexBuilder::new(
        r#"
            # Datetime of the log line.
            (?<datetime>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z)

            # Ensure it's about the `http_client` scope.
            .*matrix_sdk::http_client

            # Ensure it's about a sync.
            .*>\ssync_once\{conn_id="(?<connection_id>[^"]+)"\}

            # Let's capture some data about `send()`!
            \s>\ssend\{
                request_id="REQ-(?<request_id>\d+)"
                \smethod=(?<method>\S+)
                \suri="(?<uri>[^"]+)"
                # If there is a `request_size`.
                (.*\srequest_size="(?<request_size>[^"]+)")?
                # If this is a response, there is a `status`.
                (.*\sstatus=(?<status>\d+))?
                # If there is a `response_size`.
                (.*\sresponse_size="(?<response_size>[^"]+)")?
        "#,
    )
    .ignore_whitespace(true)
    .build()
    .expect("Failed to build the `find_sync_start regex`");

    let mut args = env::args();
    let this_bin = args.next().expect("<bin-name> is unknown, really?");

    let Some(log_path) = args.next() else {
        panic!("<log_path> is missing; try `{this_bin} <log_path> <output_path>`");
    };

    let Some(output_path) = args.next() else {
        panic!("<output_path> is missing; try `{this_bin} <log_path> <output_path>`");
    };

    let Ok(log_file) = fs::File::open(&log_path) else {
        panic!("Failed to open `{log_path}`");
    };

    let Ok(mut output_file) = fs::File::create(&output_path) else {
        panic!("Failed to create `{output_path}`");
    };

    let reader = io::BufReader::new(log_file);
    let mut number_of_analysed_lines = 0;
    let mut number_of_matched_lines = 0;
    let mut smallest_start_at = None;
    let mut largest_end_at = None;

    let mut spans: BTreeMap<ConnectionId, BTreeMap<RequestId, Span>> = BTreeMap::new();

    for line in reader.lines().enumerate().map(|(nth, line)| {
        line.unwrap_or_else(|error| {
            panic!("Failed to read line #{nth}\n{error}");
        })
    }) {
        number_of_analysed_lines += 1;

        if let Some(captures) = find_sync.captures(&line) {
            number_of_matched_lines += 1;

            let date_time = DateTime::parse_from_rfc3339(
                captures
                    .name("datetime")
                    .expect("Failed to capture `datetime`")
                    .as_str(),
            )
            .expect("Failed to parse `datetime`");
            let connection_id = captures
                .name("connection_id")
                .expect("Failed to capture `connection_id`")
                .as_str();
            let request_id = captures
                .name("request_id")
                .expect("Failed to capture `request_id`")
                .as_str()
                .parse()
                .expect("Failed to parse `request_id`");
            let method = captures
                .name("method")
                .expect("Failed to capture `method`")
                .as_str();
            let uri = captures
                .name("uri")
                .expect("Failed to capture `uri`")
                .as_str();
            let request_size = captures
                .name("request_size")
                .map(|request_size| request_size.as_str());
            let response_size = captures
                .name("response_size")
                .map(|response_size| response_size.as_str());
            let status = captures.name("status").map(|status| status.as_str());

            if let Some(smallest_start_at_inner) = smallest_start_at {
                if smallest_start_at_inner > date_time {
                    smallest_start_at = Some(date_time.clone());
                }
            } else {
                smallest_start_at = Some(date_time.clone());
            }

            if let Some(largest_end_at_inner) = largest_end_at {
                if largest_end_at_inner < date_time {
                    largest_end_at = Some(date_time.clone());
                }
            } else {
                largest_end_at = Some(date_time.clone());
            }

            let spans_for_connection_id = spans.entry(connection_id.to_owned()).or_default();

            match spans_for_connection_id.entry(request_id) {
                Entry::Vacant(entry) => {
                    entry.insert(Span {
                        status: None,
                        method: method.to_owned(),
                        uri: uri.to_owned(),
                        request_size: request_size.map(ToOwned::to_owned),
                        response_size: response_size.map(ToOwned::to_owned),
                        start_at: date_time,
                        duration: TimeDelta::zero(),
                    });
                }
                Entry::Occupied(mut entry) => {
                    let span = entry.get_mut();

                    if let Some(status) = status {
                        if let Ok(status) = status.parse() {
                            span.status = Some(status);
                        }
                    }

                    span.duration = date_time.sub(&span.start_at);

                    if let Some(request_size) = request_size {
                        span.request_size = Some(request_size.to_owned());
                    }

                    if let Some(response_size) = response_size {
                        span.response_size = Some(response_size.to_owned());
                    }
                }
            }
        }
    }

    let smallest_start_at = smallest_start_at
        .map(|date_time| date_time.timestamp_millis())
        .unwrap_or_default();
    let largest_end_at = largest_end_at
        .map(|date_time| date_time.timestamp_millis())
        .unwrap_or_default();
    let end_at = largest_end_at.saturating_sub(smallest_start_at).to_string();
    let rows = spans
        .iter()
        .map(|(connection_id, spans)| {
            spans.iter().map(
                |(
                    request_id,
                    Span {
                        status,
                        method,
                        uri,
                        request_size,
                        response_size,
                        start_at,
                        duration,
                        ..
                    },
                )| {
                    let uri_components = Url::parse(uri, None).map(|uri| uri.components());

                    format!(
                        "    <tr>
      <td><code>{connection_id}</code></td>
      <td><code>{request_id}</code></td>
      <td data-status-family=\"{status_family}\"><span>{status}</span></td>
      <td>{method}</td>
      <td>{domain}</td>
      <td>{path}</td>
      <td>{request_size}</td>
      <td>{response_size}</td>
      <td><div class=\"span\" style=\"--start-at: {start_at}; --duration: {duration}\"><span>{duration}ms</span></div></td>
    </tr>
",
                        connection_id = connection_id.clone(),
                        status = status
                            .map(|status| status.to_string())
                            .unwrap_or_else(|| "".to_owned()),
                        status_family = status
                            .map(|status| if status > 0 { status / 100 } else { 0 } )
                            .unwrap_or_default(),
                        domain = uri_components
                            .as_ref()
                            .map(|components| uri[components.host_start as usize..components.host_end as usize].to_string())
                            .unwrap_or_default(),
                        path = uri_components
                            .as_ref()
                            .map(|components| if let Some(pathname_start) = components.pathname_start { uri[pathname_start as usize..].to_string() } else { "".to_owned() })
                            .unwrap_or_default(),
                        request_size = request_size
                            .clone()
                            .map(|request_size| request_size.to_string())
                            .unwrap_or_else(|| "".to_owned()),
                        response_size = response_size
                            .clone()
                            .map(|response_size| response_size.to_string())
                            .unwrap_or_else(|| "".to_owned()),
                        start_at = start_at
                            .timestamp_millis()
                            .saturating_sub(smallest_start_at),
                        duration = duration.num_milliseconds(),
                    )
                },
            )
        })
        .flatten()
        .collect::<String>();

    let output = OUTPUT_TEMPLATE
        .replace("{end_at}", &end_at)
        .replace("{rows}", &rows);

    output_file
        .write_all(output.as_bytes())
        .expect("Failed to write the output");

    println!(
        "\nNumber of analysed log lines: {number_of_analysed_lines}\n\
        Number of matched lines: {number_of_matched_lines}\n\
        Output file: {output_path}\n\
        Done!"
    );
}

type ConnectionId = String;

type RequestId = u32;

#[derive(Debug)]
struct Span {
    status: Option<u8>,
    method: String,
    uri: String,
    request_size: Option<String>,
    response_size: Option<String>,
    start_at: DateTime<FixedOffset>,
    duration: TimeDelta,
}
