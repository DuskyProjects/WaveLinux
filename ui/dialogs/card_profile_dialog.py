"""ALSA card profile picker dialog."""

from __future__ import annotations

import queue
import threading

from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtWidgets import QComboBox, QDialog, QDialogButtonBox, QFrame, QLabel, QVBoxLayout, QWidget

from wavelinux_theme import STYLESHEET


class CardProfileDialog(QDialog):
    """Lets the user pick an ALSA card profile directly from WaveLinux."""

    def __init__(self, engine, runtime=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.runtime = runtime
        self.setWindowTitle("Sound Card Profiles")
        self.setMinimumWidth(520)
        self.setStyleSheet(STYLESHEET)
        self._combos = []

        self._layout = QVBoxLayout(self)
        self._layout.setSpacing(12)
        self._layout.setContentsMargins(20, 20, 20, 20)

        title = QLabel("🎛 Sound Card Profiles")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        self._layout.addWidget(title)

        desc = QLabel(
            "Pick the ALSA profile for each card — e.g. Analog Stereo "
            "for headphones or Pro Audio for interfaces with many channels."
        )
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        desc.setWordWrap(True)
        self._layout.addWidget(desc)

        self._loading_lbl = QLabel("Loading sound cards…")
        self._loading_lbl.setStyleSheet("color: #6b6b82; padding: 24px;")
        self._loading_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._layout.addWidget(self._loading_lbl)

        self._cards_container = QWidget()
        self._cards_layout = QVBoxLayout(self._cards_container)
        self._cards_layout.setContentsMargins(0, 0, 0, 0)
        self._cards_layout.setSpacing(10)
        self._cards_container.setVisible(False)
        self._layout.addWidget(self._cards_container)

        self._btns = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Apply | QDialogButtonBox.StandardButton.Close
        )
        self._btns.button(QDialogButtonBox.StandardButton.Apply).clicked.connect(self._apply)
        self._btns.button(QDialogButtonBox.StandardButton.Apply).setEnabled(False)
        self._btns.button(QDialogButtonBox.StandardButton.Close).clicked.connect(self.accept)
        self._layout.addWidget(self._btns)

        self._card_queue = queue.SimpleQueue()
        self._card_poll = QTimer(self)
        self._card_poll.setInterval(30)
        self._card_poll.timeout.connect(self._poll_cards)
        self._card_poll.start()
        threading.Thread(target=self._load_cards_bg, daemon=True).start()

    def _load_cards_bg(self):
        try:
            cards = self.engine.list_cards()
            self._card_queue.put(("ok", cards))
        except Exception as exc:
            self._card_queue.put(("error", str(exc)))

    def _poll_cards(self):
        try:
            msg = self._card_queue.get_nowait()
        except queue.Empty:
            return
        self._card_poll.stop()
        self._loading_lbl.setVisible(False)

        status, data = msg
        if status == "error" or not data:
            empty = QLabel("No cards reported by PipeWire.")
            empty.setStyleSheet("color: #6b6b82; padding: 24px;")
            self._cards_layout.addWidget(empty)
        else:
            for card in data:
                row = QFrame()
                row.setObjectName("fxItemFrame")
                row.setStyleSheet(
                    "QFrame#fxItemFrame {"
                    " background: rgba(255,255,255,0.03);"
                    " border: 1px solid rgba(255,255,255,0.06);"
                    " border-radius: 10px; padding: 10px; }"
                )
                rlay = QVBoxLayout(row)
                name_lbl = QLabel(card.get("description") or card["name"])
                name_lbl.setStyleSheet("color: #e0e0ee; font-weight: bold;")
                rlay.addWidget(name_lbl)
                subtle = QLabel(card["name"])
                subtle.setStyleSheet("color: #6b6b82; font-size: 10px;")
                rlay.addWidget(subtle)

                combo = QComboBox()
                for prof in card["profiles"]:
                    label = prof["description"]
                    if not prof["available"]:
                        label += "  (unavailable)"
                    combo.addItem(label, prof["name"])
                    if not prof["available"]:
                        item = combo.model().item(combo.count() - 1)
                        if item is not None:
                            item.setEnabled(False)
                idx = combo.findData(card["active_profile"])
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                rlay.addWidget(combo)
                self._cards_layout.addWidget(row)
                self._combos.append((card["name"], combo))

        self._cards_container.setVisible(True)
        self._btns.button(QDialogButtonBox.StandardButton.Apply).setEnabled(bool(self._combos))

    def _apply(self):
        for card_name, combo in self._combos:
            target = combo.currentData()
            if not target:
                continue
            if self.runtime is not None:
                self.runtime.set_card_profile(card_name, target)
            else:
                threading.Thread(
                    target=self.engine.set_card_profile,
                    args=(card_name, target),
                    daemon=True,
                ).start()
