pub(super) fn advertised_file_name(
    file: Option<&std::path::Path>,
    file_name: Option<String>,
) -> Option<String> {
    file_name.or_else(|| {
        file.and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    })
}
