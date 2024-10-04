using System.Diagnostics;
using VRC.OSCQuery;
using System.Net;

namespace mdns_sidecar {
    internal class Program {
        private static IDiscovery _discovery;
        public static IPAddress HostIP { get; set; } = IPAddress.Loopback;
        public static IPAddress OscIP { get; set; } = IPAddress.Loopback;

        public static String? VrcOscAddr;
        public static String? VrcOscQueryAddr;

        static void Main(string[] args)
        {
            var mainProcessId = 0;
            var oscPort = 0;
            var oscQueryPort = 0;
            var serviceName = "";

            // Parse args
            if (args.Length < 1)
            {
                return;
            }

            String usage = "Usage: mdns-sidecar.exe <main-process-id> [<osc-port> <osc-query-port> <service-name>]";

            if (!int.TryParse(args[0], out mainProcessId))
            {
                Console.Error.WriteLine(usage);
                return;
            }

            if (args.Length > 1)
            {
                if (!int.TryParse(args[1], out oscPort))
                {
                    Console.Error.WriteLine(usage);
                    return;
                }

                if (args.Length < 4)
                {
                    Console.Error.WriteLine(usage);
                    return;
                }

                if (!int.TryParse(args[2], out oscQueryPort))
                {
                    Console.Error.WriteLine(usage);
                    return;
                }

                serviceName = args[3];
            }

            _discovery = new MeaModDiscovery();
            _discovery.OnOscServiceAdded += OnOSCServiceDiscovery;
            _discovery.OnOscQueryServiceAdded += OnOSCQueryServiceDiscovery;

            if (oscPort != 0 && oscQueryPort != 0 && serviceName.Length > 0)
            {
                AdvertiseOSCService(serviceName, oscPort);
                AdvertiseOSCQueryService(serviceName, oscQueryPort);
            }

            var timer = new System.Timers.Timer(1000);
            timer.Elapsed += (sender, e) => ReportData();
            timer.AutoReset = true;
            timer.Enabled = true;

            WatchMainProcess(mainProcessId);
        }

        private static void OnOSCServiceDiscovery(OSCQueryServiceProfile profile)
        {
            if (!profile.name.StartsWith("VRChat-Client-")) return;
            // Log.Information("Found VRChat client. Setting OSC address.");
            String host = "127.0.0.1";
            uint port = (uint)profile.port;
            VrcOscAddr = host + ":" + port;
            ReportData();
        }

        private static void OnOSCQueryServiceDiscovery(OSCQueryServiceProfile profile)
        {
            if (!profile.name.StartsWith("VRChat-Client-")) return;
            // Log.Information("Found VRChat client. Setting OSCQuery address.");
            String host = "127.0.0.1";
            uint port = (uint)profile.port;
            VrcOscQueryAddr = host + ":" + port;
            ReportData();
        }

        private static void AdvertiseOSCQueryService(string serviceName, int port = -1)
        {
            // Get random available port if none was specified
            port = port < 0 ? NetUtils.GetAvailableTcpPort() : port;
            _discovery.Advertise(new OSCQueryServiceProfile(serviceName, HostIP, port,
                OSCQueryServiceProfile.ServiceType.OSCQuery));
        }

        private static void AdvertiseOSCService(string serviceName, int port = -1)
        {
            // Get random available port if none was specified
            port = port < 0 ? NetUtils.GetAvailableUdpPort() : port;
            _discovery.Advertise(new OSCQueryServiceProfile(serviceName, OscIP, port,
                OSCQueryServiceProfile.ServiceType.OSC));
        }

        private static void WatchMainProcess(int mainPid)
        {
            if (mainPid == 0)
            {
                new Thread(() =>
                {
                    while (true)
                    {
                        Thread.Sleep(1000);
                    }
                }).Start();
                return;
            }

            Process? mainProcess = null;
            try
            {
                mainProcess = Process.GetProcessById(mainPid);
            }
            catch (ArgumentException)
            {
                // Log.Error("Could not find main process to watch (pid=" + mainPid + "). Stopping mdns sidecar.");
                Environment.Exit(1);
                return;
            }

            new Thread(() =>
            {
                while (true)
                {
                    if (mainProcess.HasExited)
                    {
                        // Log.Information("Main process has exited. Stopping MDNS sidecar.");
                        Environment.Exit(0);
                        return;
                    }

                    Thread.Sleep(1000);
                }
            }).Start();
        }

        private static void ReportData()
        {
            if (VrcOscAddr != null)
            {
                Console.WriteLine("VRC_OSC_ADDR_DISCOVERY " + VrcOscAddr);
            }

            if (VrcOscQueryAddr != null)
            {
                Console.WriteLine("VRC_OSCQUERY_ADDR_DISCOVERY " + VrcOscQueryAddr);
            }
        }
    }
}