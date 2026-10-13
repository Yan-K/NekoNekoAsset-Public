using System;
using System.Security.Cryptography;
#if UNITY_EDITOR_WIN
using System.Runtime.InteropServices;
#endif

namespace NekoNekoAsset.Bridge
{
    // Unity's Mono SHA256.Create uses a managed implementation on Windows.
    // Keep the same streaming/digest contract with Windows' native SHA-256.
    internal static class BridgeSha256
    {
        internal static SHA256 Create()
        {
#if UNITY_EDITOR_WIN
            return new WindowsSha256();
#else
            return SHA256.Create();
#endif
        }

#if UNITY_EDITOR_WIN
        private sealed class WindowsSha256 : SHA256
        {
            private IntPtr algorithm;
            private IntPtr hash;
            private bool disposed;

            internal WindowsSha256()
            {
                HashSizeValue = 256;
                try
                {
                    Check(BCryptOpenAlgorithmProvider(out algorithm, "SHA256", null, 0));
                    Initialize();
                }
                catch { Dispose(); throw; }
            }

            public override void Initialize()
            {
                if (disposed) throw new ObjectDisposedException(nameof(WindowsSha256));
                if (hash != IntPtr.Zero) Check(BCryptDestroyHash(hash));
                hash = IntPtr.Zero;
                // Windows 7+ owns the hash object buffer until BCryptDestroyHash.
                Check(BCryptCreateHash(algorithm, out hash, IntPtr.Zero, 0, IntPtr.Zero, 0, 0));
            }

            protected override void HashCore(byte[] array, int offset, int count)
            {
                if (disposed) throw new ObjectDisposedException(nameof(WindowsSha256));
                if (count == 0) return;
                GCHandle pinned = GCHandle.Alloc(array, GCHandleType.Pinned);
                try { Check(BCryptHashData(hash, IntPtr.Add(pinned.AddrOfPinnedObject(), offset), count, 0)); }
                finally { pinned.Free(); }
            }

            protected override byte[] HashFinal()
            {
                if (disposed) throw new ObjectDisposedException(nameof(WindowsSha256));
                byte[] output = new byte[32];
                Check(BCryptFinishHash(hash, output, output.Length, 0));
                return output;
            }

            protected override void Dispose(bool disposing)
            {
                if (hash != IntPtr.Zero) BCryptDestroyHash(hash);
                hash = IntPtr.Zero;
                if (algorithm != IntPtr.Zero) BCryptCloseAlgorithmProvider(algorithm, 0);
                algorithm = IntPtr.Zero;
                disposed = true;
                base.Dispose(disposing);
            }

            private static void Check(int status)
            {
                if (status != 0) throw new CryptographicException("Windows SHA-256 failed (" + status.ToString("x8") + ").");
            }

            [DllImport("bcrypt.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
            private static extern int BCryptOpenAlgorithmProvider(out IntPtr handle, string id, string implementation, int flags);
            [DllImport("bcrypt.dll", ExactSpelling = true)]
            private static extern int BCryptCreateHash(IntPtr algorithm, out IntPtr hash, IntPtr buffer, int bufferSize, IntPtr secret, int secretSize, int flags);
            [DllImport("bcrypt.dll", ExactSpelling = true)]
            private static extern int BCryptHashData(IntPtr hash, IntPtr bytes, int count, int flags);
            [DllImport("bcrypt.dll", ExactSpelling = true)]
            private static extern int BCryptFinishHash(IntPtr hash, byte[] output, int size, int flags);
            [DllImport("bcrypt.dll", ExactSpelling = true)]
            private static extern int BCryptDestroyHash(IntPtr hash);
            [DllImport("bcrypt.dll", ExactSpelling = true)]
            private static extern int BCryptCloseAlgorithmProvider(IntPtr algorithm, int flags);
        }
#endif
    }
}
