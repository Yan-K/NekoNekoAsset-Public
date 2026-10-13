// Adapted from Yan-K/AvatarToolbox SmartPackage, commit cb58555726b9a241dd58c7812f9e4058137d74a7.
// Copyright (c) 2024 Yan-K. MIT; see package THIRD_PARTY_NOTICES.md and SOURCE_PROVENANCE.md.
using System;

namespace NekoNekoAsset.Bridge.SmartPackage
{
	public sealed class TarEntry
	{
		public string Name;
		public long Size;
		public byte TypeFlag;
		public DateTime ModifiedTime;

		public static TarEntry File(string name, long size)
		{
			return new TarEntry
			{
				Name = name,
				Size = size,
				TypeFlag = (byte)'0',
				ModifiedTime = DateTime.UtcNow
			};
		}

		public static TarEntry Directory(string name)
		{
			return new TarEntry
			{
				Name = name,
				Size = 0,
				TypeFlag = (byte)'5',
				ModifiedTime = DateTime.UtcNow
			};
		}
	}
}
