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
    padding: 12px 20px;
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
    padding: 8px 4px;
}

/* ── Channel Strip ── */
QFrame#channelStrip {
    background: rgba(22,21,36,0.85);
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 14px;
    padding: 10px 8px;
    min-width: 160px;
    max-width: 180px;
}
QFrame#channelStrip:hover {
    border: 1px solid rgba(0,229,255,0.2);
    background: rgba(26,25,42,0.95);
}

QLabel#channelIcon {
    font-size: 20px;
    padding: 2px;
}
QLabel#channelName {
    color: #e0e0ee;
    font-size: 12px;
    font-weight: 600;
    font-family: 'Outfit', sans-serif;
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
QSlider::groove:horizontal {
    background: #1a1a28;
    height: 6px;
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

QSlider::groove:vertical {
    background: #1a1a28;
    width: 6px;
    border-radius: 3px;
}
QSlider::handle:vertical {
    background: qlineargradient(x1:0,y1:0,x2:1,y2:1,
        stop:0 #00e5ff, stop:1 #7000ff);
    height: 16px;
    width: 16px;
    margin: 0 -5px;
    border-radius: 8px;
}
QSlider::handle:vertical:hover {
    background: #00e5ff;
}
QSlider::sub-page:vertical {
    background: #1a1a28;
    border-radius: 3px;
}
QSlider::add-page:vertical {
    background: qlineargradient(x1:0,y1:0,x2:0,y2:1,
        stop:0 #00e5ff, stop:0.7 #7000ff, stop:1 #7000ff);
    border-radius: 3px;
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
    padding: 16px;
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
