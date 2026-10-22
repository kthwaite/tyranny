import AppKit
import SwiftUI
import TyrannyLikesSmokeCore

/// The `:` command palette. It renders `CommandPaletteModel` and forwards keys to it.
/// The panel floats over the window content, with its centre a third of the way down.
struct CommandPaletteView: View {
    @Environment(\.themeColours) private var colours
    @Environment(\.colorScheme) private var colorScheme
    let palette: CommandPaletteModel
    let theme: ColourTheme

    private static let width: CGFloat = 560
    private static let rowHeight: CGFloat = 26
    private static let visibleRows = 8

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                // Near-transparent, so a click outside the panel lands here and closes it.
                Color.black.opacity(0.001)
                    .contentShape(Rectangle())
                    .onTapGesture { palette.close() }
                panel
                    .frame(width: Self.width)
                    .fixedSize(horizontal: false, vertical: true)
                    .position(x: geometry.size.width / 2, y: geometry.size.height / 3)
            }
        }
    }

    private var panel: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                CommandPaletteInput(palette: palette, text: palette.text)
                if palette.isRunning {
                    ProgressView().controlSize(.small)
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 9)
            if let error = palette.error {
                Text(error)
                    .font(.callout)
                    .foregroundStyle(colours.error)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 12).padding(.bottom, 8)
                    .accessibilityIdentifier("command-palette-error")
            }
            dropdown
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .shadow(color: .black.opacity(0.35), radius: 18, y: 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("command-palette")
    }

    @ViewBuilder private var dropdown: some View {
        switch palette.dropdown {
        case .rows(let rows):
            Divider()
            rowList(rows)
        case .hint(let usage, let summary):
            Divider()
            VStack(alignment: .leading, spacing: 3) {
                Text(usage).font(.system(.callout, design: .monospaced))
                Text(summary).font(.callout).foregroundStyle(.secondary)
            }
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 12).padding(.vertical, 8)
            .accessibilityIdentifier("command-palette-hint")
        case .empty:
            EmptyView()
        }
    }

    private func rowList(_ rows: [CommandPaletteModel.Row]) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                        rowView(row, highlighted: index == palette.highlighted)
                            .id(index)
                            .contentShape(Rectangle())
                            .onTapGesture {
                                palette.highlight(index)
                                palette.acceptHighlighted()
                            }
                    }
                }
            }
            .frame(height: CGFloat(min(rows.count, Self.visibleRows)) * Self.rowHeight)
            .onChange(of: palette.highlighted) { _, highlighted in
                if let highlighted { proxy.scrollTo(highlighted) }
            }
        }
        .accessibilityIdentifier("command-palette-rows")
    }

    /// The text colour on the highlighted row: light on a dark accent, dark on a light one,
    /// as on a custom table selection. The accent is resolved in this view's appearance.
    private var highlightedText: Color {
        var accent: NSColor?
        NSAppearance(named: colorScheme == .dark ? .darkAqua : .aqua)?.performAsCurrentDrawingAppearance {
            accent = theme.nsColour(.accent).usingColorSpace(.sRGB)
        }
        guard let accent else { return .white }
        let fill = RGB(red: accent.redComponent, green: accent.greenComponent, blue: accent.blueComponent)
        return Contrast.prefersLightText(on: fill) ? .white : .black
    }

    private func rowView(_ row: CommandPaletteModel.Row, highlighted: Bool) -> some View {
        let foreground = highlighted ? highlightedText : nil
        return HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(row.value)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(foreground ?? Color.primary)
            if let summary = row.summary {
                Text(summary)
                    .font(.callout)
                    .foregroundStyle(foreground?.opacity(0.8) ?? Color.secondary)
            }
            Spacer(minLength: 0)
        }
        .lineLimit(1)
        .padding(.horizontal, 12)
        .frame(height: Self.rowHeight)
        .background(highlighted ? colours.accent : Color.clear)
    }
}

/// The palette's input line. An `NSTextField`, so Tab, Return, the arrows and Esc reach the
/// palette through the field editor instead of moving focus.
private struct CommandPaletteInput: NSViewRepresentable {
    let palette: CommandPaletteModel
    /// Read by the parent's body, so an edit from the model reaches `updateNSView`.
    let text: String

    func makeCoordinator() -> Coordinator { Coordinator(palette: palette) }

    func makeNSView(context: Context) -> InputField {
        let field = InputField()
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.cell?.wraps = false
        field.font = .monospacedSystemFont(ofSize: 15, weight: .regular)
        field.stringValue = text
        field.delegate = context.coordinator
        field.setAccessibilityIdentifier("command-palette-input")
        return field
    }

    func updateNSView(_ field: InputField, context: Context) {
        context.coordinator.palette = palette
        guard field.stringValue != text else { return }
        if let editor = field.currentEditor() {
            // Setting `stringValue` mid-edit can end editing, and that closes the palette.
            editor.string = text
        } else {
            field.stringValue = text
        }
        field.moveCaretToEnd()
    }

    @MainActor
    final class Coordinator: NSObject, NSTextFieldDelegate {
        var palette: CommandPaletteModel

        init(palette: CommandPaletteModel) { self.palette = palette }

        func controlTextDidChange(_ notification: Notification) {
            guard let field = notification.object as? NSTextField else { return }
            palette.text = field.stringValue
        }

        /// Focus left the input, for example to the toolbar filter. The palette closes with it,
        /// so it is never open without focus.
        func controlTextDidEndEditing(_ notification: Notification) {
            if palette.isOpen { palette.close() }
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            switch selector {
            case #selector(NSResponder.insertTab(_:)):
                palette.acceptHighlighted()
            case #selector(NSResponder.insertBacktab(_:)):
                break   // Shift-Tab would move focus out of the palette.
            case #selector(NSResponder.insertNewline(_:)):
                let palette = palette
                Task { await palette.submitText() }
            case #selector(NSResponder.moveUp(_:)):
                palette.moveHighlight(-1)
            case #selector(NSResponder.moveDown(_:)):
                palette.moveHighlight(1)
            case #selector(NSResponder.cancelOperation(_:)):
                palette.close()
            default:
                return false
            }
            return true
        }
    }

    /// Takes focus when it joins a window, and gives it back to the previous responder when it leaves.
    final class InputField: NSTextField {
        private weak var previousResponder: NSResponder?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            previousResponder = window.firstResponder
            DispatchQueue.main.async { [weak self] in
                guard let self, let window = self.window else { return }
                window.makeFirstResponder(self)
                self.moveCaretToEnd()
            }
        }

        override func viewWillMove(toWindow newWindow: NSWindow?) {
            if newWindow == nil, let window, let editor = currentEditor(), window.firstResponder === editor {
                // A field editor is shared with this field, so it is not given focus back.
                let previous = (previousResponder as? NSView).flatMap { $0.window === window && !($0 is NSText) ? $0 : nil }
                window.makeFirstResponder(previous)
            }
            super.viewWillMove(toWindow: newWindow)
        }

        /// Puts the caret after the text instead of selecting all of it.
        func moveCaretToEnd() {
            guard let editor = currentEditor() else { return }
            editor.selectedRange = NSRange(location: (editor.string as NSString).length, length: 0)
        }
    }
}

/// Opens the palette when `:` is typed in its window. It watches key presses with a local event
/// monitor while it has a window. `:` is not a menu key equivalent, because those beat text fields;
/// the monitor lets `CommandPaletteTrigger` refuse it while any text is being edited.
struct CommandPaletteKeyProbe: NSViewRepresentable {
    /// Returns true when it opened the palette, so the key press is consumed.
    let open: () -> Bool

    func makeNSView(context: Context) -> ProbeView {
        let view = ProbeView()
        view.open = open
        return view
    }

    func updateNSView(_ view: ProbeView, context: Context) { view.open = open }

    final class ProbeView: NSView {
        var open: (() -> Bool)?
        private var monitor: Any?

        isolated deinit { if let monitor { NSEvent.removeMonitor(monitor) } }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            guard window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self, self.opensPalette(event) else { return event }
                return nil
            }
        }

        override func hitTest(_ point: NSPoint) -> NSView? { nil }

        private func opensPalette(_ event: NSEvent) -> Bool {
            let trigger = CommandPaletteTrigger.shouldOpen(
                characters: event.charactersIgnoringModifiers,
                hasCommandModifiers: !event.modifierFlags.intersection([.command, .control, .option]).isEmpty,
                isPaletteWindow: event.window === window,
                hasSheet: window?.attachedSheet != nil,
                isEditingText: window?.firstResponder is NSText)
            return trigger && open?() == true
        }
    }
}
