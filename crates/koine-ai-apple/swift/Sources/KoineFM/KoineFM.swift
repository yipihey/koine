// A C ABI of JSON strings and callbacks over Apple's Foundation Models.
// Apple's session runs the tool loop; each bridged `Tool.call` hands the call
// to Rust (`on_tool_call`) and waits for `koine_fm_tool_result`.
//
//   koine_fm_availability() -> char*                  (free with koine_fm_free)
//   koine_fm_respond(request_json, ctx, on_event, on_tool_call) -> turn_id
//   koine_fm_tool_result(handle, result_text)
//   koine_fm_cancel(turn_id)
//   koine_fm_free(char*)
//
// Every C string handed to a callback is valid only for that call. After the
// terminal event ("done" or "error") of a turn this file never touches `ctx`.
import Foundation
import FoundationModels

public typealias KoineEventCB = @convention(c) (UnsafeMutableRawPointer?, UnsafePointer<CChar>?) -> Void
public typealias KoineToolCB = @convention(c) (UnsafeMutableRawPointer?, UInt64, UnsafePointer<CChar>?) -> Void

private struct Sink: @unchecked Sendable {
    let ctx: UnsafeMutableRawPointer?
    let onEvent: KoineEventCB
    let onTool: KoineToolCB

    func emit(_ object: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: object),
              let text = String(data: data, encoding: .utf8) else { return }
        text.withCString { onEvent(ctx, $0) }
    }
}

private final class Registry: @unchecked Sendable {
    static let shared = Registry()
    private let lock = NSLock()
    private var nextTurn: UInt64 = 1
    private var nextHandle: UInt64 = 1
    private var tasks: [UInt64: Task<Void, Never>] = [:]
    private var cancelled: Set<UInt64> = []
    private var pending: [UInt64: (turn: UInt64, cont: CheckedContinuation<String, Error>)] = [:]

    func newTurn() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        defer { nextTurn += 1 }
        return nextTurn
    }
    func setTask(_ turn: UInt64, _ task: Task<Void, Never>) {
        lock.lock(); defer { lock.unlock() }
        tasks[turn] = task
    }
    func finish(_ turn: UInt64) {
        lock.lock(); defer { lock.unlock() }
        tasks[turn] = nil
        cancelled.remove(turn)
    }
    /// Park a continuation; nil when the turn was already cancelled.
    func park(turn: UInt64, _ cont: CheckedContinuation<String, Error>) -> UInt64? {
        lock.lock(); defer { lock.unlock() }
        if cancelled.contains(turn) { return nil }
        let handle = nextHandle
        nextHandle += 1
        pending[handle] = (turn, cont)
        return handle
    }
    func resume(handle: UInt64, with text: String) {
        lock.lock()
        let entry = pending.removeValue(forKey: handle)
        lock.unlock()
        entry?.cont.resume(returning: text)
    }
    func cancel(_ turn: UInt64) {
        lock.lock()
        cancelled.insert(turn)
        let task = tasks[turn]
        let handles = pending.filter { $0.value.turn == turn }.map { $0.key }
        let conts = handles.compactMap { pending.removeValue(forKey: $0)?.cont }
        lock.unlock()
        task?.cancel()
        for cont in conts { cont.resume(throwing: CancellationError()) }
    }
}

private struct BridgedTool: Tool {
    typealias Arguments = GeneratedContent
    typealias Output = String
    let name: String
    let description: String
    let parameters: GenerationSchema
    let turn: UInt64
    fileprivate let sink: Sink

    func call(arguments: GeneratedContent) async throws -> String {
        let payload: [String: Any] = [
            "id": UUID().uuidString, "name": name, "arguments": arguments.jsonString,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let json = String(data: data, encoding: .utf8) else { return "" }
        return try await withCheckedThrowingContinuation { cont in
            guard let handle = Registry.shared.park(turn: turn, cont) else {
                cont.resume(throwing: CancellationError())
                return
            }
            json.withCString { sink.onTool(sink.ctx, handle, $0) }
        }
    }
}

private struct BadRequest: Error, LocalizedError {
    var errorDescription: String? { "the request JSON could not be parsed" }
}

private func object(_ text: String) -> [String: Any]? {
    guard let data = text.data(using: .utf8) else { return nil }
    return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
}

private func schema(_ value: Any?) throws -> GenerationSchema? {
    guard let value, !(value is NSNull) else { return nil }
    let data = try JSONSerialization.data(withJSONObject: value)
    return try JSONDecoder().decode(GenerationSchema.self, from: data)
}

private func segments(_ text: String) -> [Transcript.Segment] {
    [.text(Transcript.TextSegment(content: text))]
}

@available(macOS 26, *)
private func makeSession(_ request: [String: Any], _ turn: UInt64, _ sink: Sink) throws
    -> (LanguageModelSession, GenerationSchema?)
{
    var tools: [any Tool] = []
    for entry in request["tools"] as? [[String: Any]] ?? [] {
        guard let name = entry["name"] as? String, let parameters = try schema(entry["parameters"])
        else { continue }
        tools.append(BridgedTool(
            name: name, description: entry["description"] as? String ?? "",
            parameters: parameters, turn: turn, sink: sink))
    }
    let instructions = request["instructions"] as? String
    let history = request["history"] as? [[String: Any]] ?? []
    let session: LanguageModelSession
    if history.isEmpty {
        session = LanguageModelSession(model: .default, tools: tools, instructions: instructions)
    } else {
        var entries: [Transcript.Entry] = []
        if let instructions {
            entries.append(.instructions(Transcript.Instructions(
                segments: segments(instructions),
                toolDefinitions: tools.map { Transcript.ToolDefinition(tool: $0) })))
        }
        for turn in history {
            let text = turn["text"] as? String ?? ""
            if turn["role"] as? String == "assistant" {
                entries.append(.response(Transcript.Response(assetIDs: [], segments: segments(text))))
            } else {
                entries.append(.prompt(Transcript.Prompt(segments: segments(text))))
            }
        }
        session = LanguageModelSession(model: .default, tools: tools, transcript: Transcript(entries: entries))
    }
    return (session, try schema(request["schema"]))
}

private func classify(_ error: Error) -> (String, String) {
    let message = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
    if error is CancellationError { return ("cancelled", "cancelled") }
    if let wrapped = error as? LanguageModelSession.ToolCallError { return classify(wrapped.underlyingError) }
    if #available(macOS 27, *), let e = error as? LanguageModelError {
        switch e {
        case .contextSizeExceeded: return ("context", message)
        case .rateLimited: return ("rate_limited", message)
        case .guardrailViolation: return ("guardrail", message)
        case .refusal: return ("refusal", message)
        case .timeout: return ("other", message)
        default: return ("other", message)
        }
    }
    if let e = error as? LanguageModelSession.GenerationError {
        switch e {
        case .exceededContextWindowSize: return ("context", message)
        case .assetsUnavailable: return ("unavailable", message)
        case .guardrailViolation: return ("guardrail", message)
        case .refusal: return ("refusal", message)
        case .rateLimited, .concurrentRequests: return ("rate_limited", message)
        default: return ("other", message)
        }
    }
    return ("other", message)
}

private func availability() -> [String: Any] {
    let model = SystemLanguageModel.default
    var caps: [String: Bool] = ["toolCalling": true, "guidedGeneration": true, "reasoning": false, "vision": false]
    if #available(macOS 27, *) {
        let c = model.capabilities
        caps = [
            "toolCalling": c.contains(.toolCalling), "guidedGeneration": c.contains(.guidedGeneration),
            "reasoning": c.contains(.reasoning), "vision": c.contains(.vision),
        ]
    }
    switch model.availability {
    case .available:
        return ["available": true, "capabilities": caps]
    case .unavailable(let reason):
        let why: String
        switch reason {
        case .deviceNotEligible: why = "device not eligible for Apple Intelligence"
        case .appleIntelligenceNotEnabled: why = "Apple Intelligence is not enabled"
        case .modelNotReady: why = "the on-device model is not ready"
        @unknown default: why = "unavailable"
        }
        return ["available": false, "reason": why, "capabilities": caps]
    }
}

@_cdecl("koine_fm_availability")
public func koine_fm_availability() -> UnsafeMutablePointer<CChar>? {
    let data = (try? JSONSerialization.data(withJSONObject: availability())) ?? Data("{\"available\":false}".utf8)
    return strdup(String(decoding: data, as: UTF8.self))
}

@_cdecl("koine_fm_free")
public func koine_fm_free(_ pointer: UnsafeMutablePointer<CChar>?) { free(pointer) }

@_cdecl("koine_fm_tool_result")
public func koine_fm_tool_result(_ handle: UInt64, _ text: UnsafePointer<CChar>?) {
    Registry.shared.resume(handle: handle, with: text.map { String(cString: $0) } ?? "")
}

@_cdecl("koine_fm_cancel")
public func koine_fm_cancel(_ turn: UInt64) { Registry.shared.cancel(turn) }

@_cdecl("koine_fm_respond")
public func koine_fm_respond(
    _ requestJSON: UnsafePointer<CChar>?, _ ctx: UnsafeMutableRawPointer?,
    _ onEvent: KoineEventCB, _ onTool: KoineToolCB
) -> UInt64 {
    let sink = Sink(ctx: ctx, onEvent: onEvent, onTool: onTool)
    let turn = Registry.shared.newTurn()
    let requestText = requestJSON.map { String(cString: $0) }
    let task = Task {
        defer { Registry.shared.finish(turn) }
        do {
            guard let requestText, let request = object(requestText) else { throw BadRequest() }
            guard case .available = SystemLanguageModel.default.availability else {
                sink.emit(["type": "error", "kind": "unavailable", "message": (availability()["reason"] as? String) ?? "unavailable"])
                return
            }
            let (session, responseSchema) = try makeSession(request, turn, sink)
            let prompt = request["prompt"] as? String ?? ""
            let opts = request["options"] as? [String: Any] ?? [:]
            let options = GenerationOptions(
                temperature: opts["temperature"] as? Double,
                maximumResponseTokens: opts["max_tokens"] as? Int)
            if let responseSchema {
                let response = try await session.respond(to: prompt, schema: responseSchema, options: options)
                sink.emit(["type": "done", "content": response.content.jsonString])
            } else {
                var sent = ""
                for try await snapshot in session.streamResponse(to: prompt, options: options) {
                    let full = snapshot.content
                    if full.hasPrefix(sent) {
                        let delta = String(full.dropFirst(sent.count))
                        if !delta.isEmpty { sink.emit(["type": "text", "text": delta]) }
                    }
                    sent = full
                }
                sink.emit(["type": "done", "content": sent])
            }
        } catch {
            let (kind, message) = classify(Task.isCancelled ? CancellationError() : error)
            sink.emit(["type": "error", "kind": kind, "message": message])
        }
    }
    Registry.shared.setTask(turn, task)
    return turn
}
