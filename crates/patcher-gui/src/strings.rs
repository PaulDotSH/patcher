//! Every user-facing string, with its English default. Themes override any of them under
//! `[strings]`. `{name}`, `{version}` and `{author}` are replaced with the patch's metadata.

pub const STRINGS: &[(&str, &str)] = &[
    ("next", "Next"),
    ("back", "Back"),
    ("patch", "Patch"),
    ("patch_admin", "Patch as administrator"),
    ("cancel", "Cancel"),
    ("close", "Close"),
    ("revert", "Remove mod"),
    ("launch", "Launch game"),
    ("open_folder", "Open folder"),
    ("browse", "Browse"),
    ("copy_details", "Copy details"),
    ("show_log", "Show log"),
    ("finishing", "Finishing..."),
    ("by_author", "by {author}"),
    ("version_label", "Version {version}"),
    ("welcome_heading", "Ready to install"),
    ("revert_offer", "This mod is installed in the game folder. You can remove it and restore the original files."),
    ("license_heading", "License"),
    ("license_accept", "I accept the license"),
    ("folder_heading", "Game folder"),
    ("folder_hint", "Choose the folder that contains the game."),
    ("status_checking", "Checking files..."),
    ("status_ok", "Game found, ready to patch"),
    ("status_already", "This mod is already installed"),
    ("status_mismatch", "These files do not match the game version this mod needs"),
    ("status_not_found", "The game was not found in this folder"),
    ("options_heading", "Options"),
    ("backup_label", "Keep a backup so the mod can be removed later"),
    ("progress_heading", "Installing"),
    ("progress_heading_revert", "Removing"),
    ("done_heading", "Done"),
    ("done_installed", "{name} is installed."),
    ("done_reverted", "{name} was removed and the original files restored."),
    ("done_already", "{name} is already installed."),
    ("error_heading", "Something went wrong"),
    ("cancelled", "Cancelled; nothing was changed."),
    ("elevation_declined", "Administrator access was declined."),
];

pub fn is_key(k: &str) -> bool {
    STRINGS.iter().any(|(key, _)| *key == k)
}

/// Fills `{name}`, `{version}` and `{author}`.
pub fn fill(template: &str, name: &str, version: &str, author: &str) -> String {
    template.replace("{name}", name).replace("{version}", version).replace("{author}", author)
}
