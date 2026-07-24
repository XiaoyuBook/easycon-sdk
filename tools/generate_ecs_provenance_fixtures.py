#!/usr/bin/env python3
"""Generate or verify the self-contained Phase 4 S0 ECS provenance fixtures."""

import argparse
import hashlib
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "spec" / "fixtures" / "ecs"
LEGACY_COMMIT = "11c4b992b9bce0ff977e9c587a6c0bb0d302853e"


LEGACY_FILES = {
    "src/EasyCon.Script/Binding/BuiltinCallable.cs": (
        3863,
        "7f71eecb43e559d5158df9892419e4c7fbf71f67e3bd8eba8d83274a27a140b3",
        "3184191c2f95ff30becd55ffb4700b98ebf423e7",
    ),
    "src/EasyCon2.CLI/ConsoleOutAdapter.cs": (
        2795,
        "b664e380a5da45483c02267a0255f53ca786a86e6585426ffa9e1c59a59c2980",
        "39087e9093bf596f1a3c1d44dc340dca48b20dd6",
    ),
    "src/EasyCon2/App/EasyConForm.cs": (
        48149,
        "b809f9a0f7094be5ce7b4f52c398eb66a35ce99c058da63871e632ebc194f477",
        "abf5b3ca6100dd8bb889f522ade32563f9a17707",
    ),
    "src/EasyCon2/Controls/RichLogBox.cs": (
        3110,
        "5e4a4c9f332cbb420824a46d49a54547ddd9ed41fb9b7fa3b82c346f737517e2",
        "1a3868b805e38428dc894b06d14c252fc4a44b05",
    ),
    "src/EasyCon2/Program.cs": (
        1862,
        "9acd391ffd8437fb2e673bd1f795727f2f4b443049f7d8e95d114d4bf8a2bb41",
        "db97fd22153503605a199dd9f486e85364916976",
    ),
    "src/EasyCon2/App/MainForm.cs": (
        38588,
        "3598e6fe51402605ea96a68f9d2f94eb7e72404ec8924bdd06479abf20a92956",
        "8eb46cdadda3bba8541d75f0cc0aa1adfc5ac0a5",
    ),
    "src/EasyCon2.Avalonia/Services/LogService.cs": (
        1481,
        "1e3193c547116acdea951c2954bd400820999f329ef514d0d816cafee05e4c75",
        "cd6da28f87aac42fcb48059c7c37213453444280",
    ),
    "src/EasyCon2.Avalonia.Core/Services/LogService.cs": (
        1633,
        "da7cba3985bc0dc7b4f2aadb3a32946cb69057a1872752a3fa0ac6219975aca4",
        "02eb774520641dca116d918568d57dec1f393e04",
    ),
    "test/EasyCon.Tests/EvaluatorTests.cs": (
        5660,
        "8520a35e7ee3c6bc20692c6246b55460c97487737eaa687d578bfe19e13e5d76",
        "093bd61b1868b98c3ae2b807d7a4feb634fcf55d",
    ),
    "src/EasyCon.Capture/ImgLabel.cs": (
        6307,
        "c6192e10ce09dd6e760ca22bc96d01856577756c55948f45bff5afe3a32123d4",
        "9dcaa2e09b33faf55b532e5062041be314877834",
    ),
    "src/EasyCon2.CLI/Program.cs": (
        9171,
        "a772b4f9707e253b0f5df6fdb043e28e5738db29c38ea847e633f72dc4f593e3",
        "f0b76002ad9517297f7406502e13f694fe71fe6d",
    ),
    "src/EasyCon2.Avalonia/Services/ScriptService.cs": (
        3847,
        "360df4655bf41c3c878123a0da78301bc276c0f4c50e6e00fca9f31dfe4a6b13",
        "380e7213e45b1e84b5b65606bf9027904c17d91b",
    ),
    "src/EasyCon2/Services/CaptureService.cs": (
        3201,
        "dc308f672e0c70fd9a3383f4a2a6b05710bba50e26097b1c48651158f9608be6",
        "9d16b923b7eaf1fb27e39029c46950553383a346",
    ),
    "src/EasyCon.Script/Binding/BoundBinaryOperator.cs": (
        6069,
        "6ab58cc6c9444cd6ab11d0718a4a5439ee14fa9c203ee492dda031e3d2c7e3b6",
        "db324120720288a8f2c7cd0e3f3a3b82bfb43860",
    ),
    "src/EasyCon.Script/Evaluator.cs": (
        13442,
        "5a4303a911edb352de3547db338dc54388ecd2cd83f8b68837df7f8f26c88639",
        "bd56ff85168fe2ba1019478b96949e436a1fb1ab",
    ),
}


def source_lines(*items):
    return ("\n".join(items) + "\n").encode("utf-8")


SOURCE_SNAPSHOTS = [
    (
        "builtin-print",
        "src/EasyCon.Script/Binding/BuiltinCallable.cs",
        ["BuiltinCallable.ImplPrint"],
        27,
        34,
        source_lines(
            "    public static Value ImplPrint(ImmutableArray<Value> args, IEvalContext ctx, CancellationToken token)",
            "    {",
            "        var s = args[0].AsString();",
            "        var output = s.EndsWith('\\\\') ? s[..^1] : s;",
            "        ctx.Output?.Print(output, !ctx.CancelLineBreak);",
            "        ctx.CancelLineBreak = s.EndsWith('\\\\');",
            "        return Value.Void;",
            "    }",
        ),
    ),
    (
        "cli-print-state",
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs",
        ["ConsoleOutAdapter.Print"],
        8,
        16,
        source_lines(
            "",
            "    private bool _msgNewLine = true;",
            "    private bool _msgFirstLine = true;",
            "",
            "    public void Print(string message, bool newline = true)",
            "    {",
            "        _msgNewLine = _msgNewLine && newline;",
            "        Print(message, null);",
            "    }",
        ),
    ),
    (
        "cli-print-render",
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs",
        ["ConsoleOutAdapter.Print"],
        35,
        47,
        source_lines(
            "    private void Print(string message, Color? color, bool timestamp = true)",
            "    {",
            "        if (_msgNewLine)",
            "        {",
            "            if (!_msgFirstLine)",
            "                Console.WriteLine();",
            "            _msgFirstLine = false;",
            "            if (timestamp)",
            "                ColorfulConsole.Write(DateTime.Now.ToString(\"[HH:mm:ss.fff] \"), Color.Gray);",
            "        }",
            "        ColorfulConsole.Write(message, color ?? Color.White);",
            "        _msgNewLine = true;",
            "    }",
        ),
    ),
    (
        "cli-color-write",
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs",
        ["ColorfulConsole.Write", "AnsiColors.Reset", "AnsiColors.White"],
        66,
        88,
        source_lines(
            "public static class ColorfulConsole",
            "{",
            "    public static void Write(string message, Color color)",
            "    {",
            "        var ac = AnsiColors.White;",
            "        if (color == Color.Gray)",
            "            ac = AnsiColors.Gray;",
            "        else if (color == Color.Green)",
            "            ac = AnsiColors.Green;",
            "        else if (color == Color.Orange)",
            "            ac = AnsiColors.Orange;",
            "        else if (color == Color.Red)",
            "            ac = AnsiColors.Red;",
            "",
            "",
            "        Console.Write($\"{ac}{message}{AnsiColors.Reset}\");",
            "    }",
            "}",
            "",
            "public static class AnsiColors",
            "{",
            "    public const string Reset = \"\\u001b[0m\";",
            "    public const string White = Reset;",
        ),
    ),
    (
        "cli-gray",
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs",
        ["AnsiColors.Gray"],
        94,
        94,
        source_lines("    public const string Gray = \"\\u001b[90m\";"),
    ),
    (
        "easyconform-print",
        "src/EasyCon2/App/EasyConForm.cs",
        ["EasyConForm.Print"],
        289,
        293,
        source_lines(
            "        #region IOutputAdapter / IControllerAdapter",
            "",
            "        public async void Print(string message, bool newline = true) =>",
            "            logTxtBox.Print(message, newline);",
            "",
        ),
    ),
    (
        "richlog-state",
        "src/EasyCon2/Controls/RichLogBox.cs",
        ["RichLogBox.Print"],
        5,
        12,
        source_lines(
            "internal class RichLogBox : RichTextBox",
            "{",
            "    private readonly ConcurrentQueue<Tuple<object, Color?>> _messages = new();",
            "    private readonly AutoResetEvent _wake = new(false);",
            "",
            "    private bool _msgNewLine = true;",
            "    private bool _msgFirstLine = true;",
            "    private const int MaxTextLength = 500_000;",
        ),
    ),
    (
        "richlog-print",
        "src/EasyCon2/Controls/RichLogBox.cs",
        ["RichLogBox.Print"],
        78,
        100,
        source_lines(
            "    public async void Print(string message, bool newline = true, bool timestamp = true)",
            "    {",
            "        _msgNewLine = _msgNewLine && newline;",
            "        Print(message, null, timestamp);",
            "    }",
            "",
            "    public void Print(string message, Color? color, bool timestamp = true)",
            "    {",
            "        lock (_messages)",
            "        {",
            "            if (_msgNewLine)",
            "            {",
            "                if (!_msgFirstLine)",
            "                    _messages.Enqueue(new(Environment.NewLine, null));",
            "                _msgFirstLine = false;",
            "                if (timestamp)",
            "                    _messages.Enqueue(new(DateTime.Now.ToString(\"[HH:mm:ss.fff] \"), Color.Gray));",
            "            }",
            "            _messages.Enqueue(new(message, color));",
            "            _msgNewLine = true;",
            "        }",
            "        _wake.Set();",
            "    }",
        ),
    ),
    (
        "program-main",
        "src/EasyCon2/Program.cs",
        ["Program.Main"],
        11,
        18,
        source_lines(
            "        [STAThread]",
            "        static void Main(string[] args)",
            "        {",
            "            // To customize application configuration such as set high DPI settings or default font,",
            "            // see https://aka.ms/applicationconfiguration.",
            "            ApplicationConfiguration.Initialize();",
            "            AvaloniaRuntime.EnsureInitialized();",
            "",
        ),
    ),
    (
        "program-lite-branch",
        "src/EasyCon2/Program.cs",
        ["Program.Main"],
        25,
        29,
        source_lines(
            "            try",
            "            {",
            "                var exeName = Path.GetFileNameWithoutExtension(Application.ExecutablePath);",
            "                if (exeName.EndsWith(\"lite\", StringComparison.OrdinalIgnoreCase))",
            "                    Application.Run(new App.MainForm());",
        ),
    ),
    (
        "mainform-run-entry",
        "src/EasyCon2/App/MainForm.cs",
        ["MainForm.runStopBtn_Click"],
        258,
        264,
        source_lines(
            "    #region Script Operations",
            "",
            "    private async void runStopBtn_Click(object sender, EventArgs e)",
            "    {",
            "        runStopBtn.Enabled = false;",
            "",
            "        if (!_scriptService.IsRunning)",
        ),
    ),
    (
        "mainform-run-adapter",
        "src/EasyCon2/App/MainForm.cs",
        ["MainForm.runStopBtn_Click"],
        308,
        314,
        source_lines(
            "            _state.ScriptStartTime = DateTime.Now;",
            "            _state.ScriptRunning = true;",
            "            _vpadService?.Deactivate();",
            "",
            "            var pad = new GamePadAdapter(_deviceService.Device);",
            "            _scriptService.Run(this, pad);",
            "        }",
        ),
    ),
    (
        "mainform-print",
        "src/EasyCon2/App/MainForm.cs",
        ["MainForm.Print"],
        1112,
        1115,
        source_lines(
            "    #region IOutputAdapter",
            "",
            "    public void Print(string message, bool newline = true) =>",
            "        logTxtBox.Print(message, newline);",
        ),
    ),
    (
        "avalonia-print",
        "src/EasyCon2.Avalonia/Services/LogService.cs",
        ["LogService.Print"],
        20,
        24,
        source_lines(
            "    public void Print(string message, bool newline)",
            "    {",
            "        var text = newline ? $\"[{DateTime.Now:HH:mm:ss}] {message}\\n\" : message;",
            "        Append(text);",
            "    }",
        ),
    ),
    (
        "avalonia-buffer-flush",
        "src/EasyCon2.Avalonia/Services/LogService.cs",
        ["LogService.Append", "LogService.Flush"],
        47,
        65,
        source_lines(
            "    private void Append(string text)",
            "    {",
            "        lock (_lock)",
            "        {",
            "            _buffer.Append(text);",
            "        }",
            "    }",
            "",
            "    private void Flush(object? state)",
            "    {",
            "        string chunk;",
            "        lock (_lock)",
            "        {",
            "            if (_buffer.Length == 0) return;",
            "            chunk = _buffer.ToString();",
            "            _buffer.Clear();",
            "        }",
            "        Dispatcher.UIThread.Post(() => LogAppended?.Invoke(chunk));",
            "    }",
        ),
    ),
    (
        "avalonia-core-print",
        "src/EasyCon2.Avalonia.Core/Services/LogService.cs",
        ["LogService.Print"],
        19,
        23,
        source_lines(
            "    public void Print(string message, bool newline)",
            "    {",
            "        var text = newline ? $\"[{DateTime.Now:HH:mm:ss}] {message}\\n\" : message;",
            "        lock (_lock) { _entries.Add((text, null)); }",
            "    }",
        ),
    ),
    (
        "avalonia-core-flush",
        "src/EasyCon2.Avalonia.Core/Services/LogService.cs",
        ["LogService.Flush"],
        43,
        57,
        source_lines(
            "    private void Flush(object? state)",
            "    {",
            "        (string text, string? color)[] batch;",
            "        lock (_lock)",
            "        {",
            "            if (_entries.Count == 0) return;",
            "            batch = _entries.ToArray();",
            "            _entries.Clear();",
            "        }",
            "        Dispatcher.UIThread.Post(() =>",
            "        {",
            "            foreach (var (text, color) in batch)",
            "                LogAppended?.Invoke(text, color);",
            "        });",
            "    }",
        ),
    ),
    (
        "test-mock",
        "test/EasyCon.Tests/EvaluatorTests.cs",
        ["MockOutputAdapter.Print"],
        12,
        22,
        source_lines(
            "internal sealed class MockOutputAdapter : IOutputAdapter",
            "{",
            "    public List<string> Printed { get; } = [];",
            "    public List<string> Alerted { get; } = [];",
            "",
            "    public void Print(string message, bool newline)",
            "    {",
            "        Printed.Add(newline ? message + \"\\n\" : message);",
            "    }",
            "",
            "    public void Alert(string message)",
        ),
    ),
    (
        "imglabel-search",
        "src/EasyCon.Capture/ImgLabel.cs",
        ["ImgLabel.Search"],
        159,
        161,
        source_lines(
            "    public static List<Point> Search(this ImgLabel self, Mat ss, out double md)",
            "    {",
            "        if (self.TargetWidth > self.RangeWidth || self.TargetHeight > self.RangeHeight)",
        ),
    ),
    (
        "imglabel-scale",
        "src/EasyCon.Capture/ImgLabel.cs",
        ["ImgLabel.Search"],
        174,
        188,
        source_lines(
            "            List<Point> result = new();",
            "            if (self.searchMethod == SearchMethod.TesserDetect)",
            "            {",
            "                using var target = new Mat(ss, self._target);",
            "                var rlttxt = ECSearch.FindOCR(self.ImgBase64, target, out md);",
            "                result = [new Point(self.TargetX - self.RangeX, self.TargetY - self.RangeY)];",
            "            }",
            "            else",
            "            {",
            "                byte[] imageBytes = Convert.FromBase64String(self.ImgBase64);",
            "                using var target = imageBytes.ToMat();",
            "                result = ECSearch.FindPic(range, target, self.searchMethod, out md);",
            "            }",
            "            md *= 100;",
            "",
        ),
    ),
    (
        "cli-label-start",
        "src/EasyCon2.CLI/Program.cs",
        ["externalGetters"],
        175,
        176,
        source_lines(
            "    var externalGetters = label.ToDictionary(il => il.name, il => (Func<int>)(() =>",
            "    {",
        ),
    ),
    (
        "cli-label-conversion",
        "src/EasyCon2.CLI/Program.cs",
        ["externalGetters"],
        178,
        180,
        source_lines(
            "        il.Search(cvcap!.GetMatFrame(), out var md);",
            "        return (int)md;",
            "    }));",
        ),
    ),
    (
        "avalonia-label-start",
        "src/EasyCon2.Avalonia/Services/ScriptService.cs",
        ["ScriptService.Run externalGetters"],
        77,
        78,
        source_lines(
            "                var externalGetters = label.ToDictionary(il => il.name, il => (Func<int>)(() =>",
            "                {",
        ),
    ),
    (
        "avalonia-label-conversion",
        "src/EasyCon2.Avalonia/Services/ScriptService.cs",
        ["ScriptService.Run externalGetters"],
        80,
        84,
        source_lines(
            "                    il.Search(mat, out var md);",
            "                    return (int)md;",
            "                }));",
            "",
            "                _runner.Run(_logService, pad, externalGetters, token);",
        ),
    ),
    (
        "winforms-label",
        "src/EasyCon2/Services/CaptureService.cs",
        ["CaptureService.BuildExternalGetters"],
        100,
        115,
        source_lines(
            "    public Dictionary<string, Func<int>> BuildExternalGetters()",
            "    {",
            "        if (_captureForm == null)",
            "            return [];",
            "",
            "        return _captureForm.LoadedLabels.ToDictionary(",
            "            il => il.name,",
            "            il => (Func<int>)(() =>",
            "            {",
            "                var bmp = GetCurrentFrame();",
            "                if (bmp == null) return 0;",
            "                using var mat = BitmapConverter.ToMat(bmp);",
            "                il.Search(mat, out var md);",
            "                return (int)Math.Ceiling(md);",
            "            }));",
            "    }",
        ),
    ),
    (
        "exact-round-div",
        "src/EasyCon.Script/Binding/BoundBinaryOperator.cs",
        ["BoundBinaryOperator._operators"],
        42,
        42,
        source_lines(
            "        new(TokenType.SlashI,BoundBinaryOperatorKind.RoundDiv, ScriptType.Int, (a, b) => (int)Math.Round((double)a.AsInt() / b.AsInt(), MidpointRounding.AwayFromZero)),"
        ),
    ),
    (
        "exact-xor",
        "src/EasyCon.Script/Binding/BoundBinaryOperator.cs",
        ["BoundBinaryOperator._operators"],
        46,
        46,
        source_lines(
            "        new(TokenType.XOR,BoundBinaryOperatorKind.BitwiseXor, ScriptType.Int, (a, b) => a.AsInt() ^ b.AsInt()),"
        ),
    ),
    (
        "exact-logical-operators",
        "src/EasyCon.Script/Binding/BoundBinaryOperator.cs",
        ["BoundBinaryOperator._operators"],
        61,
        66,
        source_lines(
            "        new(TokenType.LogicAnd, BoundBinaryOperatorKind.LogicalAnd,ScriptType.Bool, (v0, v1) => {",
            "            if(!v0.AsBool()) { return false; } return v1.AsBool();",
            "        }),",
            "        new(TokenType.LogicOr, BoundBinaryOperatorKind.LogicalOr,ScriptType.Bool, (v0, v1) => {",
            "            if(v0.AsBool()) { return true; } return v1.AsBool();",
            "        }),",
        ),
    ),
    (
        "exact-short-circuit",
        "src/EasyCon.Script/Evaluator.cs",
        ["Evaluator.EvaluateBinaryExpression"],
        294,
        304,
        source_lines(
            "    private Value EvaluateBinaryExpression(BoundBinaryExpression b)",
            "    {",
            "        var left = EvaluateExpression(b.Left);",
            "        if (b.Op.Kind == BoundBinaryOperatorKind.LogicalAnd && !left.AsBool()) return false;",
            "        if (b.Op.Kind == BoundBinaryOperatorKind.LogicalOr && left.AsBool()) return true;",
            "        var right = EvaluateExpression(b.Right);",
            "",
            "        Debug.Assert(left != Value.Void && right != Value.Void);",
            "",
            "        return Value.From(b.Op.Operate(left, right));",
            "    }",
        ),
    ),
]


LIMITS = [
    (1, "profile_version", "u32", 1),
    (2, "source_units", "u32", 64),
    (3, "per_source_bytes", "u64", 262144),
    (4, "bundle_bytes", "u64", 1048576),
    (5, "source_id_bytes", "u64", 256),
    (6, "identifier_bytes", "u64", 128),
    (7, "parameters", "u32", 32),
    (8, "arguments", "u32", 32),
    (9, "syntax_nesting", "u32", 64),
    (10, "functions", "u32", 256),
    (11, "symbols", "u32", 4096),
    (12, "tokens", "u32", 262144),
    (13, "ast_nodes", "u32", 131072),
    (14, "bound_nodes", "u32", 262144),
    (15, "lowered_nodes", "u32", 262144),
    (16, "instructions", "u32", 262144),
    (17, "diagnostics_per_source", "u32", 64),
    (18, "diagnostics_total", "u32", 512),
    (19, "reserved_limit_diagnostics", "u32", 1),
    (20, "call_depth", "u32", 128),
    (21, "array_cells", "u32", 16384),
    (22, "string_bytes", "u64", 262144),
    (23, "live_logical_heap_bytes", "u64", 33554432),
    (24, "output_fragment_bytes", "u64", 32768),
    (25, "output_queue_pending", "u32", 32),
    (26, "output_payload_bytes", "u64", 1048576),
    (27, "production_instruction_fuel_present", "u8", 0),
    (28, "production_output_count_present", "u8", 0),
]


CLASSIFICATION_CONTRACTS = {
    "Legacy Exact": [
        "round-div-midpoint-away-from-zero",
        "integer-xor",
        "logical-and-or-short-circuit",
        "ordinary-i32-wrap-and-shift",
        "import-bind-nop",
        "bundle-libs-loaded-without-import",
        "shared-lib-scope",
        "lib-main-visibility-and-lib-globals-first",
        "reachable-array-string-control-flow-success",
    ],
    "Corrected": [
        "for-i32-max-stop-after-upper",
        "boolean-literals-executable",
        "libs-raw-utf8-sort",
        "lf-crlf-cr-newline",
        "typed-diagnostic-failure",
        "utf8-byte-span-and-unicode-scalar-string",
        "print-continuation",
        "label-floor",
        "effect-checkpoints-and-five-way-cleanup-before-terminal",
    ],
    "v1-native": [
        "source-bundle-and-restricted-loader",
        "bom-and-program-hash",
        "pcg-and-replay",
        "monotonic-time-and-absolute-wait",
        "ecs-limits",
        "immutable-program-and-run-completion",
        "typed-and-recording-ports",
        "generic-error-projection",
        "runtime-five-way-terminal-race",
    ],
}


PROGRAM_HASHES = {
    "legacy-exact.integer-operators": (
        "d9106739fd6e1f664fc9807a4b2549d83362350e4626a15c9e0f48a0a241678b"
    ),
    "corrected.print.cli": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.winforms": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.winforms-lite": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.avalonia": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.test-mock": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "v1-native.heap-order.left-medium": (
        "c000c49eceea2d592bc1787347b10aef88d3834624b9c64ac8df3866dd7ff8bb"
    ),
    "v1-native.heap-order.medium-left": (
        "83734a419b0df485cee09a365952e0f3b255948285eb220202740121c7c5a570"
    ),
}


def json_bytes(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("utf-8")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def artifact_ref(path, data):
    return {"path": path, "bytes": len(data), "sha256": sha256(data)}


def source_identity():
    return {"role": "Main", "source_id": "main.ecs"}


def print_source():
    slash = "\\"
    return (
        "\n".join(
            [
                'PRINT "A{}"'.format(slash),
                'PRINT "B{}"'.format(slash),
                'PRINT "C"',
                'PRINT "D"',
            ]
        )
        + "\n"
    ).encode("utf-8")


def exact_source():
    slash = "\\"
    return (
        "\n".join(
            [
                "$r0 = 5 {} 2".format(slash),
                "$r1 = -5 {} 2".format(slash),
                "$r2 = 5 ^ 3",
                "$r3 = (1 == 2) and (1 / 0 == 0)",
                "$r4 = (1 == 1) or (1 / 0 == 0)",
                "$result = [$r0, $r1, $r2, $r3, $r4]",
            ]
        )
        + "\n"
    ).encode("utf-8")


def heap_source(expression):
    return (
        '$template = "'
        + ("A" * 251000)
        + '"\n'
        + "$baseline = [$template]\n"
        + "FOR $i = 1 TO 131\n"
        + "    $baseline = APPEND($baseline, $template[0:251000])\n"
        + "NEXT\n\n"
        + "FUNC LEFT($source) : string\n"
        + "    $full = $source[0:251000]\n"
        + "    RETURN $full[0:1]\n"
        + "ENDFUNC\n\n"
        + "FUNC MEDIUM($source) : string\n"
        + "    RETURN $source[0:200000]\n"
        + "ENDFUNC\n\n"
        + "$result = {}\n".format(expression)
    ).encode("utf-8")


def token(kind, value):
    return {"kind": kind, "value": value}


def emission(transport, *tokens):
    return {"transport": transport, "tokens": list(tokens)}


def adapter_call(ordinal, message, newline, emissions):
    return {
        "ordinal": ordinal,
        "message": message,
        "newline": newline,
        "emissions": emissions,
    }


PRINT_CALLS = [(1, "A", True), (2, "B", False), (3, "C", False), (4, "D", True)]


def print_trace(fixture_id, implementations):
    return {
        "schema": "easycon-sdk:legacy-print-token-trace:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "implementations": implementations,
    }


def print_expected(fixture_id):
    return {
        "schema": "easycon-sdk:print-fragments:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "fragments": [
            {"text": "A", "starts_new_line": True},
            {"text": "B", "starts_new_line": False},
            {"text": "C", "starts_new_line": False},
            {"text": "D", "starts_new_line": True},
        ],
    }


def cli_print_implementation():
    reset = token("Ansi", "\u001b[0m")
    timestamp = token("Timestamp", "[HH:mm:ss.fff] ")
    calls = [
        adapter_call(
            1,
            "A",
            True,
            [
                emission("Console.Write", token("Ansi", "\u001b[90m"), timestamp, reset),
                emission("Console.Write", reset, token("Payload", "A"), reset),
            ],
        ),
        adapter_call(
            2,
            "B",
            False,
            [emission("Console.Write", reset, token("Payload", "B"), reset)],
        ),
        adapter_call(
            3,
            "C",
            False,
            [emission("Console.Write", reset, token("Payload", "C"), reset)],
        ),
        adapter_call(
            4,
            "D",
            True,
            [
                emission("Console.WriteLine", token("HostNewLine", "platform-newline")),
                emission("Console.Write", token("Ansi", "\u001b[90m"), timestamp, reset),
                emission("Console.Write", reset, token("Payload", "D"), reset),
            ],
        ),
    ]
    return {
        "implementation": "src/EasyCon2.CLI/ConsoleOutAdapter.cs::ConsoleOutAdapter.Print",
        "adapter_calls": calls,
        "flush": [],
    }


def winforms_print_implementation(name):
    timestamp = token("Timestamp", "[HH:mm:ss.fff] ")
    wake = emission("AutoResetEvent.Set", token("WakeSignal", "_wake"))
    calls = [
        adapter_call(
            1,
            "A",
            True,
            [
                emission("UiQueueEnqueue", timestamp),
                emission("UiQueueEnqueue", token("Payload", "A")),
                wake,
            ],
        ),
        adapter_call(
            2,
            "B",
            False,
            [emission("UiQueueEnqueue", token("Payload", "B")), wake],
        ),
        adapter_call(
            3,
            "C",
            False,
            [emission("UiQueueEnqueue", token("Payload", "C")), wake],
        ),
        adapter_call(
            4,
            "D",
            True,
            [
                emission("UiQueueEnqueue", token("HostNewLine", "Environment.NewLine")),
                emission("UiQueueEnqueue", timestamp),
                emission("UiQueueEnqueue", token("Payload", "D")),
                wake,
            ],
        ),
    ]
    return {"implementation": name, "adapter_calls": calls, "flush": []}


def avalonia_calls(transport):
    timestamp = token("Timestamp", "[HH:mm:ss] ")
    return [
        adapter_call(
            1,
            "A",
            True,
            [emission(transport, timestamp, token("Payload", "A"), token("LF", "\n"))],
        ),
        adapter_call(
            2,
            "B",
            False,
            [emission(transport, token("Payload", "B"))],
        ),
        adapter_call(
            3,
            "C",
            False,
            [emission(transport, token("Payload", "C"))],
        ),
        adapter_call(
            4,
            "D",
            True,
            [emission(transport, timestamp, token("Payload", "D"), token("LF", "\n"))],
        ),
    ]


def avalonia_implementations():
    return [
        {
            "implementation": (
                "src/EasyCon2.Avalonia/Services/LogService.cs::LogService.Print"
            ),
            "adapter_calls": avalonia_calls("BufferAppend"),
            "flush": [
                emission("Dispatcher.UIThread.Post", token("UiPost", "combined-buffer")),
                emission("LogAppended.Invoke", token("Callback", "combined-buffer")),
            ],
        },
        {
            "implementation": (
                "src/EasyCon2.Avalonia.Core/Services/LogService.cs::LogService.Print"
            ),
            "adapter_calls": avalonia_calls("EntryEnqueue"),
            "flush": [
                emission("Dispatcher.UIThread.Post", token("UiPost", "entry-batch")),
                emission("LogAppended.Invoke", token("Callback", "entry-1")),
                emission("LogAppended.Invoke", token("Callback", "entry-2")),
                emission("LogAppended.Invoke", token("Callback", "entry-3")),
                emission("LogAppended.Invoke", token("Callback", "entry-4")),
            ],
        },
    ]


def test_mock_implementation():
    calls = []
    for ordinal, message, newline in PRINT_CALLS:
        tokens = [token("Payload", message)]
        if newline:
            tokens.append(token("LF", "\n"))
        calls.append(
            adapter_call(
                ordinal,
                message,
                newline,
                [emission("ListAppend", *tokens)],
            )
        )
    return {
        "implementation": (
            "test/EasyCon.Tests/EvaluatorTests.cs::MockOutputAdapter.Print"
        ),
        "adapter_calls": calls,
        "flush": [],
    }


def label_input(fixture_id):
    return {
        "schema": "easycon-sdk:label-score-input:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "normalized_score": 0.4225,
        "legacy_scale_operation": "md *= 100",
        "legacy_scaled_score": 42.25,
    }


def label_observed(fixture_id, conversion, value):
    return {
        "schema": "easycon-sdk:legacy-label-projection:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "legacy_scaled_score": 42.25,
        "conversion": conversion,
        "observed_integer": value,
    }


def label_expected(fixture_id):
    return {
        "schema": "easycon-sdk:label-floor:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "normalized_score": 0.4225,
        "formula": "floor(clamp(score, 0, 1) * 100)",
        "expected_integer": 42,
    }


def profile_document():
    return {
        "schema": "easycon-sdk:ecs-limits-profile:v1",
        "schema_version": 1,
        "hash_format_version": 1,
        "ecs_semantics_version": 1,
        "limits": [
            {"order": order, "name": name, "encoding": encoding, "value": value}
            for order, name, encoding, value in LIMITS
        ],
    }


def source_catalog():
    entries = []
    for snapshot_id, path, symbols, line_start, line_end, content in SOURCE_SNAPSHOTS:
        source_bytes, source_hash, blob = LEGACY_FILES[path]
        assert content.count(b"\n") == line_end - line_start + 1
        entries.append(
            {
                "snapshot_id": snapshot_id,
                "repo_relative_path": path,
                "symbols": symbols,
                "source_blob_bytes": source_bytes,
                "source_blob_sha256": source_hash,
                "git_blob_sha1": blob,
                "line_start": line_start,
                "line_end": line_end,
                "content": content.decode("utf-8"),
                "content_sha256": sha256(content),
            }
        )
    return {
        "schema": "easycon-sdk:legacy-source-snapshots:v1",
        "schema_version": 1,
        "legacy_commit": LEGACY_COMMIT,
        "entries": entries,
    }


def legacy_source(path, symbols, snapshot_ids):
    source_bytes, source_hash, blob = LEGACY_FILES[path]
    return {
        "repo_relative_path": path,
        "symbols": symbols,
        "source_blob_bytes": source_bytes,
        "source_blob_sha256": source_hash,
        "git_blob_sha1": blob,
        "snapshot_ids": snapshot_ids,
    }


def exact_observed(fixture_id):
    return {
        "schema": "easycon-sdk:ecs-value-trace:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "result": [3, -3, 6, False, True],
        "short_circuit_rhs_evaluated": [False, False],
    }


def exact_expected(fixture_id):
    result = exact_observed(fixture_id)
    result["classification"] = "Legacy Exact"
    return result


def heap_baseline():
    return {
        "distinct_string_allocations": 132,
        "string_bytes_each": 251000,
        "string_bytes_total": 33132000,
        "array_direct_cells": 132,
        "array_direct_cell_bytes": 1056,
        "live_bytes": 33133056,
    }


def heap_left_medium_expected(fixture_id):
    return {
        "schema": "easycon-sdk:ecs-heap-ledger:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "limit": 33554432,
        "baseline": heap_baseline(),
        "source_order": ["LEFT($template)", "MEDIUM($template)"],
        "checkpoints": [
            {"id": "baseline", "live_bytes": 33133056, "decision": "continue"},
            {"id": "left-full-copy", "live_bytes": 33384056, "decision": "continue"},
            {"id": "left-one-byte-slice", "live_bytes": 33384057, "decision": "continue"},
            {"id": "left-full-release", "live_bytes": 33133057, "decision": "continue"},
            {"id": "medium-copy", "live_bytes": 33333057, "decision": "continue"},
            {"id": "concat-reserve", "live_bytes": 33533058, "decision": "continue"},
        ],
        "canonical_peak": 33533058,
        "outcome": {"kind": "success", "result_utf8_bytes": 200001},
        "counterfactual_right_first": {
            "classification": "forbidden-negative-oracle",
            "medium_live_bytes": 33333056,
            "left_full_reserve_attempt_bytes": 33584056,
            "decision": "reject-live-logical-heap-limit",
        },
    }


def heap_medium_left_expected(fixture_id):
    return {
        "schema": "easycon-sdk:ecs-heap-ledger:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "limit": 33554432,
        "baseline": heap_baseline(),
        "source_order": ["MEDIUM($template)", "LEFT($template)"],
        "checkpoints": [
            {"id": "baseline", "live_bytes": 33133056, "decision": "continue"},
            {"id": "medium-copy", "live_bytes": 33333056, "decision": "continue"},
            {
                "id": "left-full-reserve-attempt",
                "live_bytes": 33584056,
                "decision": "reject-live-logical-heap-limit",
            },
            {
                "id": "expression-unwind",
                "live_bytes": 33133056,
                "decision": "rollback-to-entry-checkpoint",
            },
        ],
        "canonical_peak_attempt": 33584056,
        "outcome": {"kind": "limit-failure", "limit": "live_logical_heap_bytes"},
    }


def generated_files():
    files = {}

    def add_json(name, value):
        data = json_bytes(value)
        files[name] = data
        return artifact_ref(name, data)

    def add_bytes(name, data):
        files[name] = data
        return artifact_ref(name, data)

    profile = add_json("data/v1-profile.json", profile_document())
    catalog = add_json("data/legacy-source-snapshots-v1.json", source_catalog())
    records = []

    fixture_id = "legacy-exact.integer-operators"
    input_ref = add_bytes("data/{}.input.ecs".format(fixture_id), exact_source())
    observed_ref = add_json(
        "data/{}.legacy-observed.json".format(fixture_id),
        exact_observed(fixture_id),
    )
    expected_ref = add_json(
        "data/{}.v1-expected.json".format(fixture_id),
        exact_expected(fixture_id),
    )
    records.append(
        {
            "fixture_id": fixture_id,
            "provenance_class": "Legacy Exact",
            "oracle_kind": "production-source",
            "legacy_commit": LEGACY_COMMIT,
            "legacy_sources": [
                legacy_source(
                    "src/EasyCon.Script/Binding/BoundBinaryOperator.cs",
                    ["BoundBinaryOperator._operators"],
                    ["exact-round-div", "exact-xor", "exact-logical-operators"],
                ),
                legacy_source(
                    "src/EasyCon.Script/Evaluator.cs",
                    ["Evaluator.EvaluateBinaryExpression"],
                    ["exact-short-circuit"],
                ),
            ],
            "source_identity": source_identity(),
            "program_hash_v1": PROGRAM_HASHES[fixture_id],
            "program_hash_profile": profile,
            "capture_schema": "easycon-sdk:legacy-evaluation-source:v1",
            "capture_schema_version": 1,
            "normalization_schema": "easycon-sdk:ecs-value-trace:v1",
            "normalization_schema_version": 1,
            "input": input_ref,
            "legacy_observed": observed_ref,
            "v1_expected": expected_ref,
            "covered_contracts": [
                "round-div-midpoint-away-from-zero",
                "integer-xor",
                "logical-and-or-short-circuit",
            ],
            "revision_reason": (
                "No correction: v1 preserves the fixed production integer and short-circuit semantics."
            ),
        }
    )

    print_configs = [
        (
            "corrected.print.cli",
            "production-cli",
            [cli_print_implementation()],
            [
                legacy_source(
                    "src/EasyCon.Script/Binding/BuiltinCallable.cs",
                    ["BuiltinCallable.ImplPrint"],
                    ["builtin-print"],
                ),
                legacy_source(
                    "src/EasyCon2.CLI/ConsoleOutAdapter.cs",
                    [
                        "ConsoleOutAdapter.Print",
                        "ColorfulConsole.Write",
                        "AnsiColors.Reset",
                        "AnsiColors.White",
                        "AnsiColors.Gray",
                    ],
                    ["cli-print-state", "cli-print-render", "cli-color-write", "cli-gray"],
                ),
            ],
            (
                "Legacy CLI interprets the boolean as a pre-write line boundary and emits "
                "timestamp and ANSI tokens; v1 exposes four timestamp-free typed fragments."
            ),
        ),
        (
            "corrected.print.winforms",
            "production-winforms",
            [
                winforms_print_implementation(
                    "src/EasyCon2/App/EasyConForm.cs::EasyConForm.Print"
                )
            ],
            [
                legacy_source(
                    "src/EasyCon.Script/Binding/BuiltinCallable.cs",
                    ["BuiltinCallable.ImplPrint"],
                    ["builtin-print"],
                ),
                legacy_source(
                    "src/EasyCon2/App/EasyConForm.cs",
                    ["EasyConForm.Print"],
                    ["easyconform-print"],
                ),
                legacy_source(
                    "src/EasyCon2/Controls/RichLogBox.cs",
                    ["RichLogBox.Print"],
                    ["richlog-state", "richlog-print"],
                ),
            ],
            (
                "Legacy WinForms interprets the boolean as pre-write Environment.NewLine, "
                "timestamp, and UI queue emissions; v1 exposes the frozen typed fragments."
            ),
        ),
        (
            "corrected.print.winforms-lite",
            "production-winforms-lite",
            [
                winforms_print_implementation(
                    "src/EasyCon2/App/MainForm.cs::MainForm.Print"
                )
            ],
            [
                legacy_source(
                    "src/EasyCon.Script/Binding/BuiltinCallable.cs",
                    ["BuiltinCallable.ImplPrint"],
                    ["builtin-print"],
                ),
                legacy_source(
                    "src/EasyCon2/Program.cs",
                    ["Program.Main"],
                    ["program-main", "program-lite-branch"],
                ),
                legacy_source(
                    "src/EasyCon2/App/MainForm.cs",
                    ["MainForm.runStopBtn_Click", "MainForm.Print"],
                    ["mainform-run-entry", "mainform-run-adapter", "mainform-print"],
                ),
                legacy_source(
                    "src/EasyCon2/Controls/RichLogBox.cs",
                    ["RichLogBox.Print"],
                    ["richlog-state", "richlog-print"],
                ),
            ],
            (
                "The *lite entry point selects MainForm, the runner passes MainForm as output, "
                "and MainForm delegates to RichLogBox's pre-write Environment.NewLine, timestamp, "
                "and UI queue rendering; v1 uses the frozen typed fragments, while this observed "
                "artifact remains independent."
            ),
        ),
        (
            "corrected.print.avalonia",
            "production-avalonia",
            avalonia_implementations(),
            [
                legacy_source(
                    "src/EasyCon.Script/Binding/BuiltinCallable.cs",
                    ["BuiltinCallable.ImplPrint"],
                    ["builtin-print"],
                ),
                legacy_source(
                    "src/EasyCon2.Avalonia/Services/LogService.cs",
                    ["LogService.Print", "LogService.Append", "LogService.Flush"],
                    ["avalonia-print", "avalonia-buffer-flush"],
                ),
                legacy_source(
                    "src/EasyCon2.Avalonia.Core/Services/LogService.cs",
                    ["LogService.Print", "LogService.Flush"],
                    ["avalonia-core-print", "avalonia-core-flush"],
                ),
            ],
            (
                "Both Avalonia production implementations append LF after payload and add a "
                "timestamp only on true; v1 removes frontend rendering from typed fragments."
            ),
        ),
        (
            "corrected.print.test-mock",
            "test-oracle",
            [test_mock_implementation()],
            [
                legacy_source(
                    "src/EasyCon.Script/Binding/BuiltinCallable.cs",
                    ["BuiltinCallable.ImplPrint"],
                    ["builtin-print"],
                ),
                legacy_source(
                    "test/EasyCon.Tests/EvaluatorTests.cs",
                    ["MockOutputAdapter.Print"],
                    ["test-mock"],
                ),
            ],
            (
                "The legacy test mock stores trailing LF in a list and is retained only as a "
                "test oracle; v1 uses the frozen typed fragments, and the mock does not "
                "substitute for any production frontend trace."
            ),
        ),
    ]

    for fixture_id, oracle_kind, implementations, sources, reason in print_configs:
        input_ref = add_bytes(
            "data/{}.input.ecs".format(fixture_id), print_source()
        )
        observed_ref = add_json(
            "data/{}.legacy-observed.json".format(fixture_id),
            print_trace(fixture_id, implementations),
        )
        expected_ref = add_json(
            "data/{}.v1-expected.json".format(fixture_id),
            print_expected(fixture_id),
        )
        records.append(
            {
                "fixture_id": fixture_id,
                "provenance_class": "Corrected",
                "oracle_kind": oracle_kind,
                "legacy_commit": LEGACY_COMMIT,
                "legacy_sources": sources,
                "source_identity": source_identity(),
                "program_hash_v1": PROGRAM_HASHES[fixture_id],
                "program_hash_profile": profile,
                "capture_schema": "easycon-sdk:legacy-print-token-trace:v1",
                "capture_schema_version": 1,
                "normalization_schema": "easycon-sdk:print-fragments:v1",
                "normalization_schema_version": 1,
                "input": input_ref,
                "legacy_observed": observed_ref,
                "v1_expected": expected_ref,
                "covered_contracts": ["print-continuation"],
                "revision_reason": reason,
            }
        )

    label_configs = [
        (
            "corrected.label.cli",
            "production-cli",
            "truncate-toward-zero",
            42,
            [
                legacy_source(
                    "src/EasyCon.Capture/ImgLabel.cs",
                    ["ImgLabel.Search"],
                    ["imglabel-search", "imglabel-scale"],
                ),
                legacy_source(
                    "src/EasyCon2.CLI/Program.cs",
                    ["externalGetters"],
                    ["cli-label-start", "cli-label-conversion"],
                ),
            ],
            (
                "CLI truncates the 42.25 legacy scaled value to 42; v1 explicitly floors the "
                "clamped normalized 0.4225 score to 42."
            ),
        ),
        (
            "corrected.label.avalonia",
            "production-avalonia",
            "truncate-toward-zero",
            42,
            [
                legacy_source(
                    "src/EasyCon.Capture/ImgLabel.cs",
                    ["ImgLabel.Search"],
                    ["imglabel-search", "imglabel-scale"],
                ),
                legacy_source(
                    "src/EasyCon2.Avalonia/Services/ScriptService.cs",
                    ["ScriptService.Run externalGetters"],
                    ["avalonia-label-start", "avalonia-label-conversion"],
                ),
            ],
            (
                "Avalonia truncates the 42.25 legacy scaled value to 42; its production "
                "provenance remains separate from CLI even though v1 also expects 42."
            ),
        ),
        (
            "corrected.label.winforms",
            "production-winforms",
            "ceiling",
            43,
            [
                legacy_source(
                    "src/EasyCon.Capture/ImgLabel.cs",
                    ["ImgLabel.Search"],
                    ["imglabel-search", "imglabel-scale"],
                ),
                legacy_source(
                    "src/EasyCon2/Services/CaptureService.cs",
                    ["CaptureService.BuildExternalGetters"],
                    ["winforms-label"],
                ),
            ],
            (
                "WinForms applies Math.Ceiling to the 42.25 legacy scaled value and observes "
                "43; v1 instead floors the clamped normalized 0.4225 score to 42."
            ),
        ),
    ]

    for fixture_id, oracle_kind, conversion, observed, sources, reason in label_configs:
        input_ref = add_json(
            "data/{}.input.json".format(fixture_id), label_input(fixture_id)
        )
        observed_ref = add_json(
            "data/{}.legacy-observed.json".format(fixture_id),
            label_observed(fixture_id, conversion, observed),
        )
        expected_ref = add_json(
            "data/{}.v1-expected.json".format(fixture_id),
            label_expected(fixture_id),
        )
        records.append(
            {
                "fixture_id": fixture_id,
                "provenance_class": "Corrected",
                "oracle_kind": oracle_kind,
                "legacy_commit": LEGACY_COMMIT,
                "legacy_sources": sources,
                "capture_schema": "easycon-sdk:legacy-label-projection:v1",
                "capture_schema_version": 1,
                "normalization_schema": "easycon-sdk:label-floor:v1",
                "normalization_schema_version": 1,
                "input": input_ref,
                "legacy_observed": observed_ref,
                "v1_expected": expected_ref,
                "covered_contracts": ["label-floor"],
                "revision_reason": reason,
            }
        )

    native_configs = [
        (
            "v1-native.heap-order.left-medium",
            "LEFT($template) + MEDIUM($template)",
            heap_left_medium_expected,
            "success",
            (
                "No legacy comparison: ADR-0017 fixes strict left-first evaluation, the "
                "33,533,058-byte canonical peak, and success as the only v1 outcome."
            ),
        ),
        (
            "v1-native.heap-order.medium-left",
            "MEDIUM($template) + LEFT($template)",
            heap_medium_left_expected,
            "limit-failure",
            (
                "No legacy comparison: swapping only the operands makes the 33,584,056-byte "
                "reserve attempt exceed the v1 live logical heap limit."
            ),
        ),
    ]

    for fixture_id, expression, expected_factory, expected_outcome, reason in native_configs:
        input_ref = add_bytes(
            "data/{}.input.ecs".format(fixture_id), heap_source(expression)
        )
        expected_ref = add_json(
            "data/{}.v1-expected.json".format(fixture_id),
            expected_factory(fixture_id),
        )
        records.append(
            {
                "fixture_id": fixture_id,
                "provenance_class": "v1-native",
                "oracle_kind": "adr-v1-contract",
                "contract_source": {
                    "path": "docs/decisions/0017-phase-4-ecs-automation-target.md",
                    "section": "v1 live logical heap ledger",
                },
                "source_identity": source_identity(),
                "program_hash_v1": PROGRAM_HASHES[fixture_id],
                "program_hash_profile": profile,
                "capture_schema": "easycon-sdk:ecs-heap-ledger:v1",
                "capture_schema_version": 1,
                "normalization_schema": "easycon-sdk:ecs-heap-ledger:v1",
                "normalization_schema_version": 1,
                "input": input_ref,
                "v1_expected": expected_ref,
                "covered_contracts": ["ecs-limits"],
                "expected_outcome": expected_outcome,
                "revision_reason": reason,
            }
        )

    manifest = {
        "schema_version": 1,
        "milestone": "phase-4-s0-provenance-fixtures",
        "license": "GPL-3.0-only",
        "legacy_reference_commit": LEGACY_COMMIT,
        "classification_contracts": CLASSIFICATION_CONTRACTS,
        "program_hash_profile": profile,
        "legacy_source_catalog": catalog,
        "records": records,
    }
    files["manifest.json"] = json_bytes(manifest)
    return files


def relative_files():
    if not FIXTURE_DIR.exists():
        return set()
    return {
        path.relative_to(FIXTURE_DIR).as_posix()
        for path in FIXTURE_DIR.rglob("*")
        if path.is_file()
    }


def check(files):
    expected_names = set(files)
    actual_names = relative_files()
    failures = []
    if actual_names != expected_names:
        failures.append(
            "fixture file set differs: missing={}, extra={}".format(
                sorted(expected_names - actual_names),
                sorted(actual_names - expected_names),
            )
        )
    for name, expected in files.items():
        path = FIXTURE_DIR / name
        try:
            actual = path.read_bytes()
        except OSError as error:
            failures.append("{}: {}".format(path, error))
            continue
        if actual != expected:
            failures.append("{} is not reproducible from the tracked generator".format(path))
    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1
    print("validated 11 self-contained ECS provenance records and 33 artifacts")
    return 0


def write(files):
    for name, data in files.items():
        path = FIXTURE_DIR / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    print("generated 11 self-contained ECS provenance records and 33 artifacts")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify tracked fixture bytes instead of replacing them",
    )
    arguments = parser.parse_args()
    files = generated_files()
    return check(files) if arguments.check else write(files)


if __name__ == "__main__":
    sys.exit(main())
