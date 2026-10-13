using System;
using System.Collections.Concurrent;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using System.Threading;
using System.Threading.Tasks;

namespace NekoNekoAsset.Bridge
{
    internal sealed class BridgeSubmission
    {
        internal string ResponseJson;
        internal StoredBridgeRequest Request;
        internal BridgeManifest Manifest;
        internal string InputsDirectory;
    }

    // Only byte IO runs here. Callbacks marshal JSON/receipt/Unity work to main.
    internal sealed class BridgeServer : IDisposable
    {
        private readonly TcpListener listener;
        private readonly CancellationTokenSource stop = new CancellationTokenSource();
        private readonly ConcurrentDictionary<int, TcpClient> clients = new ConcurrentDictionary<int, TcpClient>();
        private readonly ConcurrentDictionary<int, string> requestByClient = new ConcurrentDictionary<int, string>();
        private readonly Func<string, BridgeSubmission> begin;
        private readonly Func<BridgeSubmission, bool, string, string> finish;
        private int nextClient;
        internal int Port => ((IPEndPoint)listener.LocalEndpoint).Port;

        internal BridgeServer(Func<string, BridgeSubmission> begin,
            Func<BridgeSubmission, bool, string, string> finish)
        {
            this.begin = begin;
            this.finish = finish;
            listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start(8);
            Task.Run(Accept);
        }

        private void Accept()
        {
            while (!stop.IsCancellationRequested)
            {
                TcpClient client;
                try { client = listener.AcceptTcpClient(); }
                catch (SocketException) { if (stop.IsCancellationRequested) return; continue; }
                catch (ObjectDisposedException) { return; }
                if (clients.Count >= 8) { client.Dispose(); continue; }
                int id = Interlocked.Increment(ref nextClient);
                clients.TryAdd(id, client);
                Task.Run(() => Serve(id, client));
            }
        }

        private void Serve(int id, TcpClient client)
        {
            BridgeSubmission submission = null;
            bool payloadComplete = false;
            try
            {
                client.NoDelay = true;
                client.ReceiveTimeout = 10000;
                client.SendTimeout = 10000;
                using (NetworkStream stream = client.GetStream())
                {
                    string json = BridgeFiles.Utf8.GetString(BridgeFiles.ReadFrame(stream));
                    submission = begin(json);
                    BridgeFiles.WriteFrame(stream, submission.ResponseJson);
                    if (submission.Request == null) return;
                    requestByClient[id] = submission.Request.receipt.requestId;
                    client.ReceiveTimeout = 30000; // idle per Read, never total transfer duration
                    Directory.CreateDirectory(submission.InputsDirectory);
                    var buffer = new byte[128 * 1024];
                    foreach (BridgeEntry entry in submission.Manifest.entries)
                    {
                        stop.Token.ThrowIfCancellationRequested();
                        if (submission.Request.cancelTransfer) throw new OperationCanceledException();
                        string path = BridgeFiles.Under(submission.InputsDirectory, entry.entryId);
                        using (var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None))
                        using (var hash = BridgeSha256.Create())
                        {
                            long remaining = entry.bytes;
                            while (remaining > 0)
                            {
                                stop.Token.ThrowIfCancellationRequested();
                                if (submission.Request.cancelTransfer) throw new OperationCanceledException();
                                int count = stream.Read(buffer, 0, (int)Math.Min(buffer.Length, remaining));
                                if (count == 0) throw new EndOfStreamException("Incomplete transfer.");
                                output.Write(buffer, 0, count);
                                hash.TransformBlock(buffer, 0, count, null, 0);
                                remaining -= count;
                            }
                            hash.TransformFinalBlock(Array.Empty<byte>(), 0, 0);
                            if (!string.Equals(BridgeFiles.Hex(hash.Hash), entry.sha256, StringComparison.Ordinal))
                                throw new InvalidDataException("Transferred file hash mismatch.");
                            output.Flush(true);
                        }
                    }
                    if (stream.ReadByte() != -1) throw new InvalidDataException("Unexpected payload bytes.");
                    // Receipt is durable before this callback returns its staged ACK.
                    string durableResponse = finish(submission, true, null);
                    payloadComplete = true;
                    BridgeFiles.WriteFrame(stream, durableResponse);
                }
            }
            catch (Exception)
            {
                if (submission != null && submission.Request != null && !payloadComplete && !stop.IsCancellationRequested)
                {
                    try { finish(submission, false, "Transfer interrupted or failed verification."); }
                    catch (Exception) { /* No ACK is evidence of a terminal state; App queries the receipt. */ }
                }
                // Never log the command/capability/private manifest or call Unity here.
            }
            finally
            {
                clients.TryRemove(id, out TcpClient _);
                requestByClient.TryRemove(id, out string _);
                client.Dispose();
            }
        }

        internal void CancelTransfer(string requestId)
        {
            foreach (var pair in requestByClient)
                if (pair.Value == requestId && clients.TryGetValue(pair.Key, out TcpClient client)) client.Dispose();
        }

        public void Dispose()
        {
            stop.Cancel();
            listener.Stop();
            foreach (TcpClient client in clients.Values) client.Dispose();
            // Workers observe cancellation; no Unity API or foreground interaction.
        }
    }
}
