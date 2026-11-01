// Minimal JSON reader for the bridge's own output.
using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

/// Minimal JSON reader for the bridge's own output (objects, arrays, strings, numbers, booleans,
/// null). It replaces JavaScriptSerializer, whose assembly (System.Web.Extensions, and System.Web
/// behind it) costs start-up time and memory. Objects become Dictionary<string, object>, arrays
/// object[], integers long, other numbers double.
internal static class MiniJson
{
    public static object Parse(string text)
    {
        int i = 0;
        object value = ReadValue(text, ref i);
        SkipSpace(text, ref i);
        if (i != text.Length) { throw new FormatException("Unexpected data after the JSON value"); }
        return value;
    }

    private static void SkipSpace(string s, ref int i)
    {
        while (i < s.Length && char.IsWhiteSpace(s[i])) { i++; }
    }

    private static char Peek(string s, int i)
    {
        if (i >= s.Length) { throw new FormatException("Unexpected end of JSON"); }
        return s[i];
    }

    private static bool Take(string s, ref int i, string word)
    {
        if (string.CompareOrdinal(s, i, word, 0, word.Length) != 0) { return false; }
        i += word.Length;
        return true;
    }

    private static object ReadValue(string s, ref int i)
    {
        SkipSpace(s, ref i);
        char c = Peek(s, i);
        if (c == '{') { return ReadObject(s, ref i); }
        if (c == '[') { return ReadArray(s, ref i); }
        if (c == '"') { return ReadString(s, ref i); }
        if (Take(s, ref i, "true")) { return true; }
        if (Take(s, ref i, "false")) { return false; }
        if (Take(s, ref i, "null")) { return null; }
        return ReadNumber(s, ref i);
    }

    private static Dictionary<string, object> ReadObject(string s, ref int i)
    {
        Dictionary<string, object> map = new Dictionary<string, object>();
        i++;
        SkipSpace(s, ref i);
        if (Peek(s, i) == '}') { i++; return map; }
        while (true)
        {
            SkipSpace(s, ref i);
            if (Peek(s, i) != '"') { throw new FormatException("Expected a property name"); }
            string key = ReadString(s, ref i);
            SkipSpace(s, ref i);
            if (Peek(s, i) != ':') { throw new FormatException("Expected ':'"); }
            i++;
            map[key] = ReadValue(s, ref i);
            SkipSpace(s, ref i);
            char next = Peek(s, i);
            i++;
            if (next == '}') { return map; }
            if (next != ',') { throw new FormatException("Expected ',' or '}'"); }
        }
    }

    private static object[] ReadArray(string s, ref int i)
    {
        List<object> items = new List<object>();
        i++;
        SkipSpace(s, ref i);
        if (Peek(s, i) == ']') { i++; return items.ToArray(); }
        while (true)
        {
            items.Add(ReadValue(s, ref i));
            SkipSpace(s, ref i);
            char next = Peek(s, i);
            i++;
            if (next == ']') { return items.ToArray(); }
            if (next != ',') { throw new FormatException("Expected ',' or ']'"); }
        }
    }

    private static string ReadString(string s, ref int i)
    {
        StringBuilder sb = new StringBuilder();
        i++;
        while (true)
        {
            char c = Peek(s, i);
            i++;
            if (c == '"') { return sb.ToString(); }
            if (c != '\\') { sb.Append(c); continue; }
            char e = Peek(s, i);
            i++;
            switch (e)
            {
                case '"': sb.Append('"'); break;
                case '\\': sb.Append('\\'); break;
                case '/': sb.Append('/'); break;
                case 'b': sb.Append('\b'); break;
                case 'f': sb.Append('\f'); break;
                case 'n': sb.Append('\n'); break;
                case 'r': sb.Append('\r'); break;
                case 't': sb.Append('\t'); break;
                case 'u':
                    if (i + 4 > s.Length) { throw new FormatException("Bad \\u escape"); }
                    sb.Append((char)int.Parse(s.Substring(i, 4), NumberStyles.HexNumber,
                        CultureInfo.InvariantCulture));
                    i += 4;
                    break;
                default: throw new FormatException("Bad escape \\" + e);
            }
        }
    }

    private static object ReadNumber(string s, ref int i)
    {
        int start = i;
        while (i < s.Length && "+-0123456789.eE".IndexOf(s[i]) >= 0) { i++; }
        if (i == start) { throw new FormatException("Unexpected character '" + s[i] + "'"); }
        string text = s.Substring(start, i - start);
        long whole;
        if (text.IndexOfAny(new[] { '.', 'e', 'E' }) < 0 &&
            long.TryParse(text, NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out whole))
        {
            return whole;
        }
        return double.Parse(text, NumberStyles.Float, CultureInfo.InvariantCulture);
    }
}
