// SPDX-License-Identifier: Apache-2.0
// A minimal incremental Server-Sent-Events parser for the streaming turn
// plane (POST /v1/sessions/{id}/turns/stream). Hand-rolled by design — the
// server's emitter is simple (named events, single-line JSON data, comment
// keep-alives) and a dependency would be heavier than the format.
//
// The parser is PURE: feed it byte chunks as they arrive (chunk boundaries
// may fall anywhere, including mid-line and mid-UTF-8-codepoint) and it
// yields complete events. Per the SSE grammar it handles `event:`/`data:`
// fields, multi-line data accumulation, \n / \r\n / \r line endings, comment
// lines (leading ':', the keep-alive form), and ignores fields it does not
// know (`id:`, `retry:`).
//
// Retention is bounded EXACTLY: a stream that never terminates its lines or
// events cannot grow client memory past MaxEventBytes — the bound is
// checked before a byte is buffered. Overflow never discards events the
// same chunk completed: Push returns them and the parser is poisoned so the
// NEXT push throws SseOverflowException (it throws immediately only when
// nothing completed), and the caller ends the stream with a typed error
// instead of buffering toward an OOM kill.

using System.Buffers;
using System.Text;

namespace Ioka.Munarium.Client;

/// <summary>One dispatched SSE event: the event name (empty = the spec's
/// default "message") and the joined data payload.</summary>
internal readonly record struct SseEvent(string Event, string Data);

/// <summary>The parser refused to buffer further — the peer sent more than
/// <see cref="SseParser.MaxEventBytes"/> without completing an event.</summary>
internal sealed class SseOverflowException()
    : Exception("SSE event exceeded the buffer cap without completing");

internal sealed class SseParser
{
    /// <summary>Upper bound on one event's buffered bytes (pending line +
    /// accumulated data). A terminal done event carries a whole TurnResult —
    /// hits text included — so the cap is generous; anything past it is a
    /// misbehaving peer, not a real event.</summary>
    internal const int MaxEventBytes = 16 * 1024 * 1024;

    private static readonly SseEvent[] None = [];

    /// <summary>Undelivered raw bytes (a partial line, possibly a partial
    /// codepoint). Reset — not reallocated — per line, so capacity is
    /// reused; whole runs are appended, never byte-by-byte.</summary>
    private readonly ArrayBufferWriter<byte> _buf = new(256);

    /// <summary>Accumulated data: lines for the event being built.</summary>
    private readonly List<string> _data = [];

    /// <summary>Bytes across _data (tracked so the cap is O(1) to enforce).</summary>
    private int _dataBytes;

    /// <summary>The pending event: name.</summary>
    private string _event = "";

    /// <summary>True when the previous byte was CR — a following LF is the
    /// same line ending, not an extra blank line.</summary>
    private bool _sawCr;

    /// <summary>Set once the cap was hit; every later push throws.</summary>
    private bool _poisoned;

    /// <summary>Feed one chunk; returns every event completed by it, in
    /// order (a shared empty list when none — do not mutate). Throws
    /// <see cref="SseOverflowException"/> past the cap, after first
    /// returning anything the chunk completed.</summary>
    public IReadOnlyList<SseEvent> Push(ReadOnlySpan<byte> chunk)
    {
        if (_poisoned) throw new SseOverflowException();
        List<SseEvent>? output = null;
        while (!chunk.IsEmpty)
        {
            if (_sawCr && chunk[0] == (byte)'\n')
            {
                _sawCr = false; // CRLF: LF consumed
                chunk = chunk[1..];
                continue;
            }
            var end = chunk.IndexOfAny((byte)'\r', (byte)'\n');
            var run = end < 0 ? chunk : chunk[..end];
            if (_buf.WrittenCount + run.Length + _dataBytes > MaxEventBytes)
            {
                _poisoned = true;
                break;
            }
            if (end < 0)
            {
                _sawCr = false;
                _buf.Write(run);
                break;
            }
            _sawCr = chunk[end] == (byte)'\r';
            Line(run, ref output);
            _buf.ResetWrittenCount();
            chunk = chunk[(end + 1)..];
        }
        if (_poisoned && output is null) throw new SseOverflowException();
        return output ?? (IReadOnlyList<SseEvent>)None;
    }

    /// <summary>Handle one complete line: the pending buffer plus
    /// <paramref name="tail"/> (the part of it that arrived in this chunk).</summary>
    private void Line(ReadOnlySpan<byte> tail, ref List<SseEvent>? output)
    {
        ReadOnlySpan<byte> line;
        if (_buf.WrittenCount == 0)
        {
            line = tail; // whole line in this chunk: no copy
        }
        else
        {
            _buf.Write(tail);
            line = _buf.WrittenSpan;
        }
        if (line.IsEmpty)
        {
            // Blank line = dispatch. An event with no data lines is dropped
            // per the SSE spec (this is what makes comment keep-alives free).
            if (_data.Count > 0)
            {
                (output ??= []).Add(new SseEvent(_event, string.Join('\n', _data)));
                _data.Clear();
            }
            _event = "";
            _dataBytes = 0;
            return;
        }
        if (line[0] == (byte)':') return; // comment / keep-alive

        var colon = line.IndexOf((byte)':');
        var field = colon < 0 ? line : line[..colon];
        var value = colon < 0 ? default : line[(colon + 1)..];
        if (!value.IsEmpty && value[0] == (byte)' ') value = value[1..];
        // Only the value is decoded; the field name is matched as bytes.
        if (field.SequenceEqual("event"u8))
        {
            _event = Encoding.UTF8.GetString(value);
        }
        else if (field.SequenceEqual("data"u8))
        {
            _dataBytes += value.Length;
            _data.Add(Encoding.UTF8.GetString(value));
        }
        // id / retry / unknown fields: ignored
    }
}
