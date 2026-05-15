use super::*;

impl Compositor {
    pub fn push_palette(&mut self, palette: Palette) {
        self.palette = Some(palette);
    }

    pub fn palette_mut(&mut self) -> Option<&mut Palette> {
        self.palette.as_mut()
    }

    pub fn pop_palette(&mut self) -> Option<Palette> {
        self.palette.take()
    }

    pub fn set_markdown_link_hover_candidates(&mut self, candidates: Vec<String>) {
        if candidates.is_empty() {
            self.markdown_link_hover = None;
            return;
        }
        if let Some(hover) = self.markdown_link_hover.as_mut() {
            hover.set_candidates(candidates);
            if hover.is_empty() {
                self.markdown_link_hover = None;
            }
        } else {
            self.markdown_link_hover = Some(MarkdownLinkHover::new(candidates));
        }
    }

    pub fn close_markdown_link_hover(&mut self) {
        self.markdown_link_hover = None;
    }

    pub fn can_show_markdown_link_hover(&self) -> bool {
        self.palette.is_none()
            && self.git_view.is_none()
            && self.commit_log.is_none()
            && self.pr_list_picker.is_none()
            && self.issue_list_picker.is_none()
            && self.explorer_popup.is_none()
            && self.project_root_popup.is_none()
            && self.recent_project_popup.is_none()
            && self.save_as_popup.is_none()
            && self.find_replace_popup.is_none()
            && self.search_bar.is_none()
            && self.explorer.is_none()
    }

    pub fn open_git_view(&mut self, view: GitView) {
        self.git_view = Some(view);
    }

    pub fn git_view_mut(&mut self) -> Option<&mut GitView> {
        self.git_view.as_mut()
    }

    pub fn close_git_view(&mut self) {
        self.git_view = None;
    }

    pub fn has_git_view(&self) -> bool {
        self.git_view.is_some()
    }

    pub fn open_commit_log(&mut self, view: CommitLogView) {
        self.commit_log = Some(view);
    }

    pub fn commit_log_mut(&mut self) -> Option<&mut CommitLogView> {
        self.commit_log.as_mut()
    }

    pub fn close_commit_log(&mut self) {
        self.commit_log = None;
    }

    pub fn has_commit_log(&self) -> bool {
        self.commit_log.is_some()
    }

    pub fn open_pr_list_picker(&mut self, picker: PrListPicker) {
        self.pr_list_picker = Some(picker);
    }

    pub fn close_pr_list_picker(&mut self) {
        self.pr_list_picker = None;
    }

    pub fn open_issue_list_picker(&mut self, picker: IssueListPicker) {
        self.issue_list_picker = Some(picker);
    }

    pub fn close_issue_list_picker(&mut self) {
        self.issue_list_picker = None;
    }

    pub fn open_explorer_popup(&mut self, popup: ExplorerPopup) {
        self.explorer_popup = Some(popup);
    }

    pub fn explorer_popup_mut(&mut self) -> Option<&mut ExplorerPopup> {
        self.explorer_popup.as_mut()
    }

    pub fn close_explorer_popup(&mut self) {
        self.explorer_popup = None;
    }

    pub fn has_explorer_popup(&self) -> bool {
        self.explorer_popup.is_some()
    }

    pub fn open_project_root_popup(&mut self, popup: ProjectRootPopup) {
        self.project_root_popup = Some(popup);
    }

    pub fn close_project_root_popup(&mut self) {
        self.project_root_popup = None;
    }

    pub fn has_project_root_popup(&self) -> bool {
        self.project_root_popup.is_some()
    }

    pub fn open_recent_project_popup(&mut self, popup: RecentProjectPopup) {
        self.recent_project_popup = Some(popup);
    }

    pub fn close_recent_project_popup(&mut self) {
        self.recent_project_popup = None;
    }

    pub fn has_recent_project_popup(&self) -> bool {
        self.recent_project_popup.is_some()
    }

    pub fn open_save_as_popup(&mut self, popup: SaveAsPopup) {
        self.save_as_popup = Some(popup);
    }

    pub fn close_save_as_popup(&mut self) {
        self.save_as_popup = None;
    }

    pub fn has_save_as_popup(&self) -> bool {
        self.save_as_popup.is_some()
    }

    pub fn save_as_popup_input(&self) -> Option<&str> {
        self.save_as_popup.as_ref().map(|popup| popup.input())
    }

    pub fn open_find_replace_popup(&mut self, popup: FindReplacePopup) {
        self.find_replace_popup = Some(popup);
    }

    pub fn close_find_replace_popup(&mut self) {
        self.find_replace_popup = None;
    }

    pub fn find_replace_popup_mut(&mut self) -> Option<&mut FindReplacePopup> {
        self.find_replace_popup.as_mut()
    }

    pub fn update_command_helper(&mut self, key_state: &KeyState) {
        match key_state {
            KeyState::Normal => self.command_helper = None,
            _ => self.command_helper = Some(CommandHelper::new(key_state)),
        }
    }

    pub fn open_explorer(&mut self, explorer: Explorer) {
        self.explorer = Some(explorer);
    }

    pub fn explorer_mut(&mut self) -> Option<&mut Explorer> {
        self.explorer.as_mut()
    }

    pub fn close_explorer(&mut self) -> Option<Explorer> {
        self.explorer.take()
    }

    pub fn has_explorer(&self) -> bool {
        self.explorer.is_some()
    }
}
