-- Folder names were stored as the server sends them, in modified UTF-7 (RFC 3501 §5.1.3). The hook this migration
-- runs in store.rs (`decode_folder_names`) renames them to UTF-8, which SQL cannot decode.
SELECT 1;
