import AppKit
import SwiftUI
import TyrannyLikesSmokeCore

/// The key bindings: each chord with its command, Edit and Remove, Add Binding… and Reset to Defaults.
struct KeysSettingsPane: View {
    @Environment(\.themeColours) private var colours
    let keymap: KeymapStore
    @State private var editor: KeymapEditor?
    @State private var confirmingReset = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Keyboard Shortcuts").font(.title2)
            List(keymap.bindings, id: \.chord) { binding in
                HStack {
                    Text(binding.chord.display).frame(width: 90, alignment: .leading)
                    Text(binding.command).font(.system(.body, design: .monospaced))
                    if binding.isCustom { Text("Custom").font(.caption).foregroundStyle(.secondary) }
                    if (try? CommandLanguage.parse(binding.command)) == nil {
                        Text("This command no longer parses.").font(.caption).foregroundStyle(colours.warning)
                    }
                    Spacer()
                    Button("Edit") { editor = KeymapEditor(store: keymap, editing: binding) }
                        .accessibilityIdentifier("keys-edit-\(binding.chord.canonical)")
                    Button("Remove") { keymap.remove(binding.chord) }
                        .accessibilityIdentifier("keys-remove-\(binding.chord.canonical)")
                }
            }
            .frame(height: 300)
            .accessibilityIdentifier("keys-list")
            HStack {
                Button("Add Binding…") { editor = KeymapEditor(store: keymap) }
                    .accessibilityIdentifier("keys-add")
                Spacer()
                Button("Reset to Defaults") { confirmingReset = true }
                    .accessibilityIdentifier("keys-reset")
            }
        }
        .padding(20).frame(width: 600)
        .confirmationDialog("Reset all key bindings?", isPresented: $confirmingReset) {
            Button("Reset to Defaults", role: .destructive) { keymap.resetAll() }
        } message: {
            Text("Your changes to the keys are discarded.")
        }
        .sheet(isPresented: Binding(get: { editor != nil }, set: { if !$0 { editor = nil } })) {
            if let editor {
                KeymapEditorSheet(editor: editor) { self.editor = nil }
            }
        }
    }
}

/// Record a chord, type a command, see why it cannot be saved.
private struct KeymapEditorSheet: View {
    @Environment(\.themeColours) private var colours
    @Bindable var editor: KeymapEditor
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Key Binding").font(.title3)
            LabeledContent("Key") {
                KeyRecorder(chord: $editor.chord).frame(width: 160)
            }
            TextField("Command", text: $editor.command)
                .font(.system(.body, design: .monospaced))
                .accessibilityIdentifier("keys-command")
            if let message {
                Text(message).font(.caption).foregroundStyle(colours.error)
                    .accessibilityIdentifier("keys-verdict")
            }
            HStack {
                Spacer()
                Button("Cancel", action: close).keyboardShortcut(.cancelAction)
                Button(editor.saveTitle) {
                    editor.save()
                    close()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!editor.canSave)
                .accessibilityIdentifier("keys-save")
            }
        }
        .padding(20).frame(width: 420)
    }

    private var message: String? {
        switch editor.verdict {
        case .invalidCommand(let text), .reserved(let text): text
        case .incomplete, .ready, .conflict:
            editor.conflictingCommand.map {
                "\(editor.chord?.display ?? "This key") already runs “\($0)”. Saving replaces it."
            }
        }
    }
}

/// A button that records the next key press as a chord.
///
/// While recording, a local monitor consumes every key in the recorder's window, ⌘ chords
/// included, so no menu item or bound command fires. Escape cancels recording. A key in another
/// window, or in a text field being edited, stops recording and goes on to its target.
private struct KeyRecorder: NSViewRepresentable {
    @Binding var chord: KeyChord?

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> NSButton {
        let button = NSButton(title: "", target: context.coordinator, action: #selector(Coordinator.toggle))
        button.bezelStyle = .rounded
        button.setAccessibilityIdentifier("keys-recorder")
        return button
    }

    func updateNSView(_ button: NSButton, context: Context) {
        context.coordinator.chord = $chord
        context.coordinator.button = button
        context.coordinator.refresh()
    }

    static func dismantleNSView(_ button: NSButton, coordinator: Coordinator) { coordinator.stop() }

    @MainActor final class Coordinator: NSObject {
        var chord: Binding<KeyChord?>?
        weak var button: NSButton?
        private var monitor: Any?

        isolated deinit { if let monitor { NSEvent.removeMonitor(monitor) } }

        @objc func toggle() {
            if monitor == nil { start() } else { stop() }
        }

        func refresh() {
            button?.title = monitor != nil ? "Press a key…" : chord?.wrappedValue?.display ?? "Click to record"
        }

        private func start() {
            // A focused Command field gives up focus, so its keys no longer count as text editing.
            button?.window?.makeFirstResponder(nil)
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self else { return event }
                let isEscape = event.keyCode == 53
                    && event.modifierFlags.intersection([.command, .control, .option, .shift]).isEmpty
                switch KeyRecording.action(
                    isRecorderWindow: event.window === button?.window,
                    isEditingText: button?.window?.isEditingText ?? false,
                    isEscape: isEscape)
                {
                case .abandon:
                    stop()
                    return event
                case .cancel:
                    stop()
                case .record:
                    if let recorded = event.keyChord {
                        chord?.wrappedValue = recorded
                        stop()
                    }
                }
                return nil
            }
            refresh()
        }

        func stop() {
            if let monitor { NSEvent.removeMonitor(monitor) }
            monitor = nil
            refresh()
        }
    }
}
