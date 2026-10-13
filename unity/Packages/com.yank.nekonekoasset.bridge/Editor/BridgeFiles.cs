using System;
using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text;

namespace NekoNekoAsset.Bridge
{
    internal static class BridgeFiles
    {
        internal const int MaximumControlBytes = 16 * 1024 * 1024;
        internal const int MaximumStoredBytes = 128 * 1024 * 1024;
        internal static readonly UTF8Encoding Utf8 = new UTF8Encoding(false, true);

        internal static string Now() => DateTime.UtcNow.ToString("o", CultureInfo.InvariantCulture);

        internal static string Diagnostic(string value, int maximumBytes)
        {
            if (value == null) return null;
            var result = new StringBuilder();
            int bytes = 0;
            for (int i = 0; i < value.Length; ++i)
            {
                char c = value[i];
                if (char.IsControl(c)) c = ' ';
                bool pair = char.IsHighSurrogate(c) && i + 1 < value.Length && char.IsLowSurrogate(value[i + 1]);
                if (char.IsSurrogate(c) && !pair) c = '?';
                int size = pair ? 4 : c < 0x80 ? 1 : c < 0x800 ? 2 : 3;
                if (bytes + size > maximumBytes) break;
                result.Append(c);
                if (pair) result.Append(value[++i]);
                bytes += size;
            }
            return result.ToString();
        }

        internal static bool IsUuid(string value) => value != null
            && Guid.TryParseExact(value, "D", out Guid _);

        internal static bool IsHash(string value)
        {
            if (value == null || value.Length != 64) return false;
            foreach (char c in value)
                if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false;
            return true;
        }

        internal static string Hash(byte[] bytes)
        {
            using (var hash = BridgeSha256.Create()) return Hex(hash.ComputeHash(bytes));
        }

        internal static string HashFile(string path, Action<long> progress = null, Func<bool> cancelled = null)
        {
            using (var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 64 * 1024, FileOptions.SequentialScan))
            using (var hash = BridgeSha256.Create())
            {
                byte[] buffer = new byte[64 * 1024];
                long readBytes = 0;
                int read;
                while ((read = stream.Read(buffer, 0, buffer.Length)) > 0)
                {
                    if (cancelled != null && cancelled()) throw new OperationCanceledException();
                    hash.TransformBlock(buffer, 0, read, null, 0);
                    readBytes += read;
                    progress?.Invoke(readBytes);
                }
                hash.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
                return Hex(hash.Hash);
            }
        }

        internal static string Hex(byte[] bytes)
        {
            var builder = new StringBuilder(bytes.Length * 2);
            foreach (byte b in bytes) builder.Append(b.ToString("x2", CultureInfo.InvariantCulture));
            return builder.ToString();
        }

        internal static string RandomCapability()
        {
            var bytes = new byte[32];
            using (var rng = RandomNumberGenerator.Create()) rng.GetBytes(bytes);
            return Hex(bytes);
        }

        internal static bool EqualCapability(string left, string right)
        {
            if (left == null || right == null || left.Length != 64 || right.Length != 64) return false;
            int difference = 0;
            for (int i = 0; i < 64; ++i) difference |= left[i] ^ right[i];
            return difference == 0;
        }

        internal static string SafeRelative(string value)
        {
            if (string.IsNullOrEmpty(value) || value.Length > 4096 || value != value.Replace('\\', '/')
                || Path.IsPathRooted(value)) throw new InvalidDataException("Invalid relative path.");
            foreach (string part in value.Split('/'))
            {
                if (part.Length == 0 || part == "." || part == ".." || part.EndsWith(".") || part.EndsWith(" ")
                    || part.IndexOfAny(new[] { ':', '\0', '<', '>', '"', '|', '?', '*' }) >= 0)
                    throw new InvalidDataException("Invalid relative path.");
                foreach (char c in part) if (char.IsControl(c)) throw new InvalidDataException("Invalid relative path.");
                string stem = part.Split('.')[0].ToUpperInvariant();
                if (stem == "CON" || stem == "PRN" || stem == "AUX" || stem == "NUL"
                    || (stem.Length == 4 && (stem.StartsWith("COM") || stem.StartsWith("LPT"))
                        && stem[3] >= '1' && stem[3] <= '9'))
                    throw new InvalidDataException("Reserved filename.");
            }
            return value;
        }

        internal static void RejectLinks(string path)
        {
            for (string current = Path.GetFullPath(path); !string.IsNullOrEmpty(current); current = Path.GetDirectoryName(current))
            {
                if ((File.Exists(current) || Directory.Exists(current))
                    && (File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                    throw new IOException("Linked paths are not allowed.");
            }
        }

        internal static string Under(string root, string relative)
        {
            SafeRelative(relative);
            string prefix = Path.GetFullPath(root).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
            string path = Path.GetFullPath(Path.Combine(root, relative));
            if (!path.StartsWith(prefix, Path.DirectorySeparatorChar == '\\' ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal))
                throw new IOException("Path leaves its owned directory.");
            RejectLinks(path);
            return path;
        }

        internal static void AtomicWrite(string path, string text)
        {
            RejectLinks(path);
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            string temp = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                byte[] bytes = Utf8.GetBytes(text);
                using (var stream = new FileStream(temp, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                {
                    stream.Write(bytes, 0, bytes.Length);
                    stream.Flush(true);
                }
                if (File.Exists(path)) File.Replace(temp, path, null);
                else File.Move(temp, path);
            }
            finally { if (File.Exists(temp)) File.Delete(temp); }
        }

        internal static byte[] ReadFrame(Stream stream)
        {
            var prefix = new byte[4];
            ReadExact(stream, prefix, 0, 4);
            uint length = (uint)(prefix[0] | prefix[1] << 8 | prefix[2] << 16 | prefix[3] << 24);
            if (length == 0 || length > MaximumControlBytes) throw new InvalidDataException("Invalid control length.");
            var bytes = new byte[(int)length];
            ReadExact(stream, bytes, 0, bytes.Length);
            return bytes;
        }

        internal static void ReadExact(Stream stream, byte[] bytes, int offset, int count)
        {
            while (count > 0)
            {
                int read = stream.Read(bytes, offset, count);
                if (read == 0) throw new EndOfStreamException("Incomplete transfer.");
                offset += read;
                count -= read;
            }
        }

        internal static void WriteFrame(Stream stream, string json)
        {
            byte[] bytes = Utf8.GetBytes(json);
            if (bytes.Length > MaximumControlBytes) throw new InvalidDataException("Response exceeds control limit.");
            stream.WriteByte((byte)bytes.Length);
            stream.WriteByte((byte)(bytes.Length >> 8));
            stream.WriteByte((byte)(bytes.Length >> 16));
            stream.WriteByte((byte)(bytes.Length >> 24));
            stream.Write(bytes, 0, bytes.Length);
            stream.Flush();
        }
    }
}
