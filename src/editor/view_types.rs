/// View type identifiers for the type-checking system that replaces core.object.
/// Each view class has a unique ID and a parent chain for extends() checks.
///
/// Unique view type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewType {
    Object,
    View,
    EmptyView,
    TitleView,
    LogView,
    ToolbarView,
    StatusView,
    CommandView,
    NagView,
    ContextMenu,
    DocView,
    TreeView,
    Node,
    RootView,
    Scrollbar,
    Dialog,
    // Plugin-created view types
    MarkdownPreview,
    GitDiffView,
    GitStatusView,
    TerminalView,
    ProjectSearchResults,
    ProjectReplaceView,
}

impl ViewType {
    /// Parent type in the inheritance chain.
    pub fn parent(self) -> Option<ViewType> {
        match self {
            ViewType::Object => None,
            ViewType::View => Some(ViewType::Object),
            ViewType::Scrollbar => Some(ViewType::Object),
            _ => Some(ViewType::View),
        }
    }

    /// Check if this type is exactly `target`.
    pub fn is(self, target: ViewType) -> bool {
        self == target
    }

    /// Check if this type extends `target` (walks the parent chain).
    pub fn extends(self, target: ViewType) -> bool {
        if self == target {
            return true;
        }
        let mut current = self.parent();
        while let Some(p) = current {
            if p == target {
                return true;
            }
            current = p.parent();
        }
        false
    }

    /// String identifier for Lua interop.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            ViewType::Object => "Object",
            ViewType::View => "View",
            ViewType::EmptyView => "EmptyView",
            ViewType::TitleView => "TitleView",
            ViewType::LogView => "LogView",
            ViewType::ToolbarView => "ToolbarView",
            ViewType::StatusView => "StatusView",
            ViewType::CommandView => "CommandView",
            ViewType::NagView => "NagView",
            ViewType::ContextMenu => "ContextMenu",
            ViewType::DocView => "DocView",
            ViewType::TreeView => "TreeView",
            ViewType::Node => "Node",
            ViewType::RootView => "RootView",
            ViewType::Scrollbar => "Scrollbar",
            ViewType::Dialog => "Dialog",
            ViewType::MarkdownPreview => "MarkdownPreview",
            ViewType::GitDiffView => "GitDiffView",
            ViewType::GitStatusView => "GitStatusView",
            ViewType::TerminalView => "TerminalView",
            ViewType::ProjectSearchResults => "ProjectSearchResults",
            ViewType::ProjectReplaceView => "ProjectReplaceView",
        }
    }

    /// Look up a ViewType from its string name.
    pub fn from_str(s: &str) -> Option<ViewType> {
        match s {
            "Object" => Some(ViewType::Object),
            "View" => Some(ViewType::View),
            "EmptyView" => Some(ViewType::EmptyView),
            "TitleView" => Some(ViewType::TitleView),
            "LogView" => Some(ViewType::LogView),
            "ToolbarView" => Some(ViewType::ToolbarView),
            "StatusView" => Some(ViewType::StatusView),
            "CommandView" => Some(ViewType::CommandView),
            "NagView" => Some(ViewType::NagView),
            "ContextMenu" => Some(ViewType::ContextMenu),
            "DocView" => Some(ViewType::DocView),
            "TreeView" => Some(ViewType::TreeView),
            "Node" => Some(ViewType::Node),
            "RootView" => Some(ViewType::RootView),
            "Scrollbar" => Some(ViewType::Scrollbar),
            "Dialog" => Some(ViewType::Dialog),
            "MarkdownPreview" => Some(ViewType::MarkdownPreview),
            "GitDiffView" => Some(ViewType::GitDiffView),
            "GitStatusView" => Some(ViewType::GitStatusView),
            "TerminalView" => Some(ViewType::TerminalView),
            "ProjectSearchResults" => Some(ViewType::ProjectSearchResults),
            "ProjectReplaceView" => Some(ViewType::ProjectReplaceView),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_checks_exact_type() {
        assert!(ViewType::DocView.is(ViewType::DocView));
        assert!(!ViewType::DocView.is(ViewType::View));
    }

    #[test]
    fn extends_walks_chain() {
        assert!(ViewType::DocView.extends(ViewType::View));
        assert!(ViewType::DocView.extends(ViewType::Object));
        assert!(ViewType::DocView.extends(ViewType::DocView));
        assert!(!ViewType::View.extends(ViewType::DocView));
    }

    #[test]
    fn scrollbar_extends_object_not_view() {
        assert!(ViewType::Scrollbar.extends(ViewType::Object));
        assert!(!ViewType::Scrollbar.extends(ViewType::View));
    }

    #[test]
    fn round_trip_str() {
        for vt in [
            ViewType::Object,
            ViewType::View,
            ViewType::DocView,
            ViewType::TreeView,
        ] {
            assert_eq!(ViewType::from_str(vt.as_str()), Some(vt));
        }
    }
}
