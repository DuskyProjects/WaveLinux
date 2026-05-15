"""Small status card used by the Health settings tab."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QVBoxLayout

from health import HealthIssue


class HealthCard(QFrame):
    """Small status card used by the Health settings tab."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("healthCard")
        self.setProperty("severity", "info")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(8)

        head = QHBoxLayout()
        head.setContentsMargins(0, 0, 0, 0)
        head.setSpacing(8)

        self.badge_lbl = QLabel("INFO")
        self.badge_lbl.setObjectName("healthBadge")
        self.badge_lbl.setProperty("severity", "info")
        head.addWidget(self.badge_lbl, 0, Qt.AlignmentFlag.AlignTop)

        self.title_lbl = QLabel()
        self.title_lbl.setObjectName("healthTitle")
        self.title_lbl.setWordWrap(True)
        head.addWidget(self.title_lbl, 1)
        layout.addLayout(head)

        self.detail_lbl = QLabel()
        self.detail_lbl.setObjectName("healthDetail")
        self.detail_lbl.setWordWrap(True)
        layout.addWidget(self.detail_lbl)

        action_row = QHBoxLayout()
        action_row.setContentsMargins(0, 0, 0, 0)
        action_row.setSpacing(8)
        self.primary_btn = QPushButton()
        self.primary_btn.setObjectName("showHiddenBtn")
        action_row.addWidget(self.primary_btn)
        self.secondary_btn = QPushButton()
        self.secondary_btn.setObjectName("showHiddenBtn")
        action_row.addWidget(self.secondary_btn)
        action_row.addStretch()
        layout.addLayout(action_row)

    def configure(self, issue: HealthIssue, *, primary_handler=None, secondary_handler=None):
        severity = (issue.severity or "info").strip().lower()
        badge_text = severity.upper()
        self.setProperty("severity", severity)
        self.badge_lbl.setText(badge_text)
        self.badge_lbl.setProperty("severity", severity)
        self.title_lbl.setText(issue.title or issue.code)
        self.detail_lbl.setText(issue.detail or "")
        self._refresh_style()

        self._configure_button(self.primary_btn, issue.primary_action, primary_handler)
        self._configure_button(self.secondary_btn, issue.secondary_action, secondary_handler)

    @staticmethod
    def _disconnect_button(button):
        try:
            button.clicked.disconnect()
        except TypeError:
            pass

    def _configure_button(self, button, text, handler):
        self._disconnect_button(button)
        text = str(text or "").strip()
        visible = bool(text and handler is not None)
        button.setVisible(visible)
        if not visible:
            return
        button.setText(text)
        button.clicked.connect(handler)

    def _refresh_style(self):
        for widget in (self, self.badge_lbl):
            widget.style().unpolish(widget)
            widget.style().polish(widget)
