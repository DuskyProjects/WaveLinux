STYLESHEET = """
QMainWindow {
    background-color: #0d0d14;
}
QWidget#central {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:1,
        stop:0 #0d0d14, stop:0.5 #12111f, stop:1 #0d0d14);
}

/* ── Header ── */
QFrame#header {
    background: rgba(18,17,31,0.9);
    border-bottom: 1px solid rgba(255,255,255,0.06);
    padding: 6px 16px;
}
QLabel#logoLabel {
    color: #ffffff;
    font-size: 22px;
    font-weight: 800;
    font-family: 'Outfit', 'Segoe UI', sans-serif;
}
QLabel#subtitleLabel {
    color: #6b6b82;
    font-size: 11px;
    font-family: 'Inter', sans-serif;
}

/* ── Section Labels ── */
QLabel#sectionLabel {
    color: #5a5a72;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 2px;
    font-family: 'Outfit', sans-serif;
    padding: 3px 4px;
}

/* ── Health Center ── */
QFrame#healthCard {
    background: rgba(20, 20, 32, 0.92);
    border: 1px solid rgba(255,255,255,0.08);
    border-radius: 12px;
}
QFrame#healthCard[severity="ok"] {
    border: 1px solid rgba(0,212,170,0.25);
    background: rgba(12, 34, 32, 0.78);
}
QFrame#healthCard[severity="info"] {
    border: 1px solid rgba(0,229,255,0.2);
}
QFrame#healthCard[severity="warning"] {
    border: 1px solid rgba(210,139,38,0.35);
    background: rgba(42, 30, 18, 0.84);
}
QFrame#healthCard[severity="error"] {
    border: 1px solid rgba(255,107,107,0.4);
    background: rgba(44, 20, 28, 0.9);
}
QLabel#healthBadge {
    border-radius: 10px;
    padding: 2px 8px;
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 1px;
    font-family: 'Outfit', sans-serif;
}
QLabel#healthBadge[severity="ok"] {
    background: rgba(0,212,170,0.18);
    color: #00d4aa;
}
QLabel#healthBadge[severity="info"] {
    background: rgba(0,229,255,0.16);
    color: #00e5ff;
}
QLabel#healthBadge[severity="warning"] {
    background: rgba(210,139,38,0.2);
    color: #d28b26;
}
QLabel#healthBadge[severity="error"] {
    background: rgba(255,107,107,0.2);
    color: #ff8f8f;
}
QLabel#healthTitle {
    color: #f2f2fa;
    font-size: 13px;
    font-weight: 700;
    font-family: 'Outfit', sans-serif;
}
QLabel#healthDetail {
    color: #9ea0b8;
    font-size: 11px;
    line-height: 1.4;
}

/* ── Channel Strip ── */
QFrame#channelStrip {
    background: rgba(22,21,36,0.85);
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 14px;
    padding: 6px 6px;
}
QFrame#channelStrip[degraded="true"] {
    background: rgba(44,20,28,0.92);
    border: 1px solid rgba(255,107,107,0.45);
}
QFrame#channelStrip:hover {
    border: 1px solid rgba(0,229,255,0.2);
    background: rgba(26,25,42,0.95);
}
QFrame#channelStrip[degraded="true"]:hover {
    border: 1px solid rgba(255,140,140,0.65);
    background: rgba(54,24,34,0.96);
}

QLabel#channelIcon {
    font-size: 22px;
    padding: 2px;
}
QLabel#healthIndicator {
    color: #ff6b6b;
    font-size: 13px;
    font-weight: 700;
    padding: 0 2px;
}
QLabel#fxIndicator {
    color: #00e5ff;
    font-size: 14px;
    padding: 0 2px;
}
QLabel#channelName {
    color: #e0e0ee;
    font-size: 12px;
    font-weight: 600;
    font-family: 'Outfit', sans-serif;
}
QLabel#mixTagMon {
    color: #00e5ff;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 2px;
}
QLabel#mixTagStr {
    color: #7000ff;
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 2px;
}
QLabel#masterMixLabel {
    color: #e0e0ee;
    font-size: 12px;
    font-weight: 700;
    font-family: 'Outfit', sans-serif;
    padding-right: 8px;
}
QLabel#streamHintLabel {
    color: #7000ff;
    font-size: 11px;
    font-weight: 600;
}
QPushButton#linkBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 12px;
    font-size: 11px;
}
QPushButton#linkBtn:hover {
    background: rgba(0,229,255,0.1);
    color: #00e5ff;
}
QPushButton#linkBtn:checked {
    background: rgba(0,229,255,0.2);
    border: 1px solid #00e5ff;
    color: #00e5ff;
}
QLabel#channelType {
    color: #00e5ff;
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 1px;
    font-family: 'Inter', sans-serif;
}
QLabel#volumeLabel {
    color: #8b8b9e;
    font-size: 11px;
    font-family: 'Inter', sans-serif;
    font-weight: 500;
}
QLabel#mixLabel {
    color: #a0a0b8;
    font-size: 9px;
    font-weight: bold;
}

/* ── Sliders ── */
QSlider:horizontal {
    min-height: 18px;
}
QSlider::groove:horizontal {
    background: #1a1a28;
    height: 6px;
    margin: 0 8px;
    border-radius: 3px;
}
QSlider::handle:horizontal {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:1,
        stop:0 #00e5ff, stop:1 #7000ff);
    width: 16px;
    height: 16px;
    margin: -5px 0;
    border-radius: 8px;
}
QSlider::handle:horizontal:hover {
    background: #00e5ff;
}
QSlider::sub-page:horizontal {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:0,
        stop:0 #00e5ff, stop:0.7 #7000ff, stop:1 #7000ff);
    border-radius: 3px;
}
QSlider::add-page:horizontal {
    background: #1a1a28;
    border-radius: 3px;
}

QSlider:vertical {
    min-width: 16px;
}
QSlider::groove:vertical {
    background: #1a1a28;
    width: 4px;
    margin: 0 6px;
    border-radius: 2px;
}
QSlider::handle:vertical {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:1,
        stop:0 #00e5ff, stop:1 #7000ff);
    height: 16px;
    width: 16px;
    margin: 0 -6px;
    border-radius: 8px;
}
QSlider::handle:vertical:hover {
    background: #00e5ff;
}
QSlider::sub-page:vertical {
    background: #1a1a28;
    width: 4px;
    margin: 0 6px;
    border-radius: 2px;
}
QSlider::add-page:vertical {
    background: qlineargradient(x1:0,y1:0,x2:0,y2:1,
        stop:0 #00e5ff, stop:0.7 #7000ff, stop:1 #7000ff);
    width: 4px;
    margin: 0 6px;
    border-radius: 2px;
}

/* ── Buttons ── */
QPushButton#muteBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 8px;
    padding: 4px;
    font-size: 12px;
    min-height: 24px;
}
QPushButton#muteBtn:hover {
    background: rgba(255,255,255,0.1);
}
QPushButton#muteBtn[muted="true"] {
    background: rgba(255,51,102,0.2);
    border: 1px solid #ff3366;
    color: #ff3366;
}

QPushButton#fxBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 8px;
    padding: 5px;
    font-size: 11px;
    min-height: 24px;
}
QPushButton#fxBtn:hover {
    background: rgba(112,0,255,0.15);
}
QPushButton#fxBtn[active="true"] {
    background: rgba(0,229,255,0.15);
    border: 1px solid #00e5ff;
    color: #00e5ff;
}

QPushButton#addBtn {
    background: #00e5ff;
    color: #000;
    font-weight: 700;
    font-family: 'Outfit', sans-serif;
    border: none;
    border-radius: 8px;
    padding: 10px 20px;
    font-size: 13px;
}
QPushButton#addBtn:hover {
    background: #33ebff;
}
QPushButton#addBtn:pressed {
    background: #00bcd4;
}

QPushButton#removeBtn {
    background: transparent;
    border: 1px solid rgba(255,51,102,0.3);
    color: #ff3366;
    border-radius: 6px;
    padding: 4px 8px;
    font-size: 10px;
}
QPushButton#removeBtn:hover {
    background: rgba(255,51,102,0.15);
}

QPushButton#resetBtn {
    background: rgba(255,51,102,0.12);
    border: 1px solid rgba(255,51,102,0.35);
    color: #ff6a8a;
    border-radius: 8px;
    padding: 10px 16px;
    font-size: 12px;
    font-weight: 600;
    font-family: 'Outfit', sans-serif;
}
QPushButton#resetBtn:hover {
    background: rgba(255,51,102,0.22);
    color: #ffffff;
}
QPushButton#resetBtn:pressed {
    background: rgba(255,51,102,0.35);
}

/* ── Solo / Clipguard / Scene controls ── */
QPushButton#soloBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #e0e0ee;
    border-radius: 8px;
    font-weight: 700;
    font-size: 11px;
}
QPushButton#soloBtn:hover {
    background: rgba(255,204,0,0.18);
    color: #ffd64d;
}
QPushButton#soloBtn:checked {
    background: rgba(255,204,0,0.28);
    border: 1px solid #ffcc00;
    color: #ffcc00;
}

QPushButton#clipguardBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 8px;
    padding: 6px 12px;
    font-size: 11px;
    font-weight: 600;
}
QPushButton#clipguardBtn:hover {
    background: rgba(0,229,255,0.1);
}
QPushButton#clipguardBtn:checked {
    background: rgba(0,229,255,0.18);
    border: 1px solid #00e5ff;
    color: #00e5ff;
}

QLabel#scenePickerLabel {
    color: #5a5a72;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 2px;
    font-family: 'Outfit', sans-serif;
    padding-right: 6px;
}
QComboBox#sceneCombo {
    min-width: 150px;
}
QPushButton#sceneSaveBtn, QPushButton#sceneDelBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 8px;
    padding: 6px;
    font-size: 13px;
}
QPushButton#sceneSaveBtn:hover {
    background: rgba(0,229,255,0.15);
    color: #00e5ff;
}
QPushButton#sceneDelBtn:hover {
    background: rgba(255,51,102,0.15);
    color: #ff6a8a;
}

QPushButton#forgetBtn {
    background: transparent;
    border: 1px solid rgba(255,255,255,0.08);
    color: #5a5a72;
    border-radius: 6px;
    padding: 4px;
    font-size: 11px;
    font-weight: 600;
}
QPushButton#forgetBtn:hover {
    background: rgba(255,51,102,0.12);
    border: 1px solid rgba(255,51,102,0.3);
    color: #ff6a8a;
}
QPushButton#forgetBtn:disabled {
    color: #2a2a3a;
    border: 1px solid rgba(255,255,255,0.04);
}

QPushButton#presetBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #a0a0b8;
    border-radius: 6px;
    padding: 3px 10px;
    font-size: 10px;
}
QPushButton#presetBtn:hover {
    background: rgba(0,229,255,0.12);
    color: #00e5ff;
    border: 1px solid rgba(0,229,255,0.3);
}

QPushButton#reorderBtn {
    background: transparent;
    border: none;
    color: #5a5a72;
    font-size: 11px;
    padding: 0;
}
QPushButton#reorderBtn:hover {
    color: #00e5ff;
}

/* ── Peak / VU meter ── */
QProgressBar#peakBar {
    background: rgba(255,255,255,0.04);
    border: 1px solid rgba(255,255,255,0.05);
    border-radius: 3px;
}
QProgressBar#peakBar::chunk {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:0,
        stop:0 #00e5ff, stop:0.6 #7000ff, stop:0.85 #ffcc00, stop:1 #ff3366);
    border-radius: 3px;
}

QPushButton#hideBtn {
    background: transparent;
    border: none;
    color: #5a5a72;
    font-size: 10px;
    padding: 2px;
}
QPushButton#hideBtn:hover {
    color: #00e5ff;
}

QPushButton#showHiddenBtn {
    background: rgba(255,255,255,0.05);
    border: 1px solid rgba(255,255,255,0.1);
    color: #8b8b9e;
    border-radius: 8px;
    padding: 8px 14px;
    font-size: 12px;
    font-family: 'Outfit', sans-serif;
}
QPushButton#showHiddenBtn:hover {
    background: rgba(255,255,255,0.1);
    color: #e0e0ee;
}
QPushButton#showHiddenBtn:checked {
    background: rgba(0,229,255,0.15);
    border: 1px solid #00e5ff;
    color: #00e5ff;
}

/* ── App Routing ── */
QFrame#routingPanel {
    background: rgba(22,21,36,0.7);
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 14px;
    padding: 8px;
}
QLabel#routingTitle {
    color: #e0e0ee;
    font-size: 14px;
    font-weight: 600;
    font-family: 'Outfit', sans-serif;
}
QComboBox {
    background: #1a1a28;
    color: #e0e0ee;
    border: 1px solid rgba(255,255,255,0.1);
    border-radius: 6px;
    padding: 6px 10px;
    font-size: 12px;
    min-width: 120px;
}
QComboBox:hover {
    border: 1px solid rgba(0,229,255,0.3);
}
QComboBox::drop-down {
    border: none;
    padding-right: 8px;
}
QComboBox QAbstractItemView {
    background: #1a1a28;
    color: #e0e0ee;
    border: 1px solid rgba(255,255,255,0.1);
    selection-background-color: rgba(0,229,255,0.2);
}

/* ── Divider ── */
QFrame#divider {
    background: rgba(255,255,255,0.06);
    max-width: 1px;
    min-width: 1px;
}

/* ── Scroll ── */
QScrollArea#centralScroll {
    border: none;
    background: transparent;
}
QScrollBar:horizontal {
    background: transparent;
    height: 8px;
    margin: 0px 10px 0px 10px;
}
QScrollBar::handle:horizontal {
    background: rgba(255,255,255,0.1);
    border-radius: 4px;
    min-width: 30px;
}
QScrollBar::handle:horizontal:hover {
    background: rgba(0,229,255,0.3);
}
QScrollBar:vertical {
    background: transparent;
    width: 8px;
    margin: 10px 0px 10px 0px;
}
QScrollBar::handle:vertical {
    background: rgba(255,255,255,0.1);
    border-radius: 4px;
    min-height: 30px;
}
QScrollBar::handle:vertical:hover {
    background: rgba(0,229,255,0.3);
}
QScrollBar::add-line, QScrollBar::sub-line {
    border: none;
    background: none;
}

/* ── Dialog ── */
QDialog {
    background: #12111f;
}
QDialog QLabel {
    color: #e0e0ee;
    font-family: 'Outfit', sans-serif;
}
QDialog QLineEdit {
    background: #1a1a28;
    color: #fff;
    border: 1px solid rgba(255,255,255,0.1);
    border-radius: 8px;
    padding: 10px;
    font-size: 14px;
}
QDialog QLineEdit:focus {
    border: 1px solid #00e5ff;
}

/* ── RNNoise Badge ── */
QLabel#rnBadge {
    background: rgba(112,0,255,0.25);
    color: #d8b4ff;
    border: 1px solid rgba(112,0,255,0.4);
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 9px;
    font-weight: 600;
}

/* ── Status Bar ── */
QFrame#statusBar {
    background: rgba(18,17,31,0.9);
    border-top: 1px solid rgba(255,255,255,0.06);
    padding: 6px 16px;
}
QLabel#statusLabel {
    color: #5a5a72;
    font-size: 10px;
    font-family: 'Inter', sans-serif;
}
QLabel#statusDot {
    color: #00ff88;
    font-size: 10px;
}
"""
