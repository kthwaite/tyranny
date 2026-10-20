import Foundation

/// Holds the reply of the one list request the client sends early at launch.
/// The first fetch whose request equals the prefetch's takes the reply; every other fetch
/// (or any later one) gets nil and sends its own request.
public actor LaunchPrefetchSlot {
    private enum State {
        case pending
        case waiting(CheckedContinuation<Data?, Never>)
        case ready(Data?)
        case claimed
    }

    private let request: [String: JSONValue]
    private var state = State.pending
    private var timer: Task<Void, Never>?

    /// `request` is the prefetch's request fields (everything but `command` and `id`).
    public init(request: [String: JSONValue]) {
        self.request = request
    }

    /// The prefetch finished: the raw ok reply body, or nil for any failure. Later calls are ignored.
    public func fulfil(_ reply: Data?) {
        switch state {
        case .pending:
            state = .ready(reply)
        case .waiting(let continuation):
            state = .claimed
            continuation.resume(returning: reply)
        case .ready, .claimed:
            break
        }
    }

    /// One-shot. Returns the reply when this is the first claim, `request` equals the prefetch's
    /// request, and the prefetch succeeds by `deadline`; otherwise nil. The slot is empty afterwards.
    public func claim(_ request: [String: JSONValue], until deadline: ContinuousClock.Instant) async -> Data? {
        let held = state
        switch held {
        case .claimed, .waiting:
            return nil
        case .ready(let reply):
            state = .claimed
            return request == self.request ? reply : nil
        case .pending:
            guard request == self.request else {
                state = .claimed
                return nil
            }
            let reply = await withCheckedContinuation { (continuation: CheckedContinuation<Data?, Never>) in
                state = .waiting(continuation)
                timer = Task { [weak self] in
                    try? await Task.sleep(until: deadline, clock: .continuous)
                    await self?.expire()
                }
            }
            timer?.cancel()
            timer = nil
            return reply
        }
    }

    /// The deadline passed while a claim waits: give up. A no-op once `fulfil` already resumed it.
    private func expire() {
        guard case .waiting(let continuation) = state else { return }
        state = .claimed
        continuation.resume(returning: nil)
    }
}
