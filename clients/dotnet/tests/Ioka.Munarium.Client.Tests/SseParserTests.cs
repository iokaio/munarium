// SPDX-License-Identifier: Apache-2.0
// Offline unit tests for the SSE parser — a faithful port of the Rust
// client's sse.rs suite: named events + keep-alives, adversarial chunk
// boundaries (byte-by-byte, CR/LF split), multi-line data, dropped
// data-less events with stale-name cleanup, ignored unknown fields, and the
// 16 MiB overflow guard in both its shapes.

using System.Text;
using Xunit;

namespace Ioka.Munarium.Client.Tests;

public class SseParserTests
{
    private static IReadOnlyList<SseEvent> One(SseParser parser, string bytes) =>
        parser.Push(Encoding.UTF8.GetBytes(bytes));

    [Fact]
    public void ParsesNamedEventsAndKeepalives()
    {
        var p = new SseParser();
        var events = One(p, ": keep-alive\n\nevent: progress\ndata: {\"stage\":\"merge\"}\n\n");
        var ev = Assert.Single(events);
        Assert.Equal("progress", ev.Event);
        Assert.Equal("{\"stage\":\"merge\"}", ev.Data);
    }

    [Fact]
    public void SurvivesArbitraryChunkBoundaries()
    {
        // The transport may split anywhere — including mid-line and between
        // CR and LF. Byte-by-byte is the adversarial version of that.
        var wire = Encoding.UTF8.GetBytes(
            "event: progress\r\ndata: {\"n\":1}\r\n\r\nevent: done\ndata: {}\n\n");
        var p = new SseParser();
        var events = new List<SseEvent>();
        foreach (var b in wire)
        {
            events.AddRange(p.Push([b]));
        }
        Assert.Equal(2, events.Count);
        Assert.Equal("progress", events[0].Event);
        Assert.Equal("{\"n\":1}", events[0].Data);
        Assert.Equal("done", events[1].Event);
    }

    [Fact]
    public void MultiLineDataJoinsWithNewlines()
    {
        var p = new SseParser();
        var events = One(p, "data: a\ndata: b\n\n");
        Assert.Equal("a\nb", events[0].Data);
        Assert.Equal("", events[0].Event); // default event name is empty
    }

    [Fact]
    public void EventWithoutDataIsDroppedNotDispatched()
    {
        var p = new SseParser();
        Assert.Empty(One(p, "event: progress\n\n"));
        // ...and the stale name does not leak into the next event.
        var events = One(p, "data: x\n\n");
        Assert.Equal("", events[0].Event);
    }

    [Fact]
    public void UnknownFieldsAndNoColonLinesAreIgnored()
    {
        var p = new SseParser();
        var events = One(p, "id: 7\nretry: 100\nnonsense\ndata: ok\n\n");
        var ev = Assert.Single(events);
        Assert.Equal("ok", ev.Data);
    }

    [Fact]
    public void ANeverendingEventOverflowsInsteadOfGrowingForever()
    {
        // No newline at all: the pending line buffer hits the cap.
        var p = new SseParser();
        var chunk = new byte[1024 * 1024];
        Array.Fill(chunk, (byte)'x');
        var overflowed = false;
        for (var i = 0; i < 20; i++)
        {
            try
            {
                p.Push(chunk);
            }
            catch (SseOverflowException)
            {
                overflowed = true;
                break;
            }
        }
        Assert.True(overflowed, "unterminated line must trip MaxEventBytes");

        // Data lines that never dispatch (no blank line) also count.
        p = new SseParser();
        var line = Encoding.UTF8.GetBytes($"data: {new string('y', 1024 * 1024)}\n");
        overflowed = false;
        for (var i = 0; i < 20; i++)
        {
            try
            {
                p.Push(line);
            }
            catch (SseOverflowException)
            {
                overflowed = true;
                break;
            }
        }
        Assert.True(overflowed, "undispatched data must trip MaxEventBytes");
    }
}
