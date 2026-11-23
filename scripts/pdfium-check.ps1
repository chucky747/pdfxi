# Independent PDF inspection through the PDFium C API (P/Invoke), used to check
# files written by PDFXI without going through PDFXI's own parser.
# Dot-source after setting $script:PdfiumDll, or let it default to the real
# per-user install:
#   $script:PdfiumDll = 'C:\path\to\pdfium.dll'
#   . (Join-Path $PSScriptRoot 'pdfium-check.ps1')
# PDFium here is the bundled third-party engine; PDFXI's write path (pdfxi-core,
# pdfxi-security, pdfxi-forms, pdfxi-edit) is separate code, so this is an
# independent reader for everything PDFXI writes.

Add-Type -AssemblyName System.Security

if (-not ('RvPdf' -as [type])) {
Add-Type @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class RvPdf {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr LoadLibrary(string path);
    [DllImport("pdfium.dll")] public static extern void FPDF_InitLibrary();
    [DllImport("pdfium.dll")] public static extern IntPtr FPDF_LoadMemDocument(IntPtr data, int size, [MarshalAs(UnmanagedType.LPStr)] string password);
    [DllImport("pdfium.dll")] public static extern uint FPDF_GetLastError();
    [DllImport("pdfium.dll")] public static extern void FPDF_CloseDocument(IntPtr doc);
    [DllImport("pdfium.dll")] public static extern int FPDF_GetPageCount(IntPtr doc);
    [DllImport("pdfium.dll")] public static extern uint FPDF_GetDocPermissions(IntPtr doc);
    [DllImport("pdfium.dll")] public static extern int FPDF_GetSecurityHandlerRevision(IntPtr doc);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDF_LoadPage(IntPtr doc, int index);
    [DllImport("pdfium.dll")] public static extern void FPDF_ClosePage(IntPtr page);
    [DllImport("pdfium.dll")] public static extern double FPDF_GetPageWidth(IntPtr page);
    [DllImport("pdfium.dll")] public static extern double FPDF_GetPageHeight(IntPtr page);
    [DllImport("pdfium.dll")] public static extern int FPDFPage_GetRotation(IntPtr page);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFText_LoadPage(IntPtr page);
    [DllImport("pdfium.dll")] public static extern void FPDFText_ClosePage(IntPtr tp);
    [DllImport("pdfium.dll")] public static extern int FPDFText_CountChars(IntPtr tp);
    [DllImport("pdfium.dll")] public static extern int FPDFText_GetText(IntPtr tp, int start, int count, byte[] buffer);
    [DllImport("pdfium.dll")] public static extern int FPDFPage_GetAnnotCount(IntPtr page);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFPage_GetAnnot(IntPtr page, int index);
    [DllImport("pdfium.dll")] public static extern void FPDFPage_CloseAnnot(IntPtr annot);
    [DllImport("pdfium.dll")] public static extern int FPDFAnnot_GetSubtype(IntPtr annot);
    [DllImport("pdfium.dll")] public static extern uint FPDFAnnot_GetStringValue(IntPtr annot, [MarshalAs(UnmanagedType.LPStr)] string key, byte[] buffer, uint buflen);
    [DllImport("pdfium.dll")] public static extern int FPDFAnnot_GetFlags(IntPtr annot);
    [DllImport("pdfium.dll")] public static extern uint FPDF_GetMetaText(IntPtr doc, [MarshalAs(UnmanagedType.LPStr)] string tag, byte[] buffer, uint buflen);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFBookmark_GetFirstChild(IntPtr doc, IntPtr bookmark);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFBookmark_GetNextSibling(IntPtr doc, IntPtr bookmark);
    [DllImport("pdfium.dll")] public static extern uint FPDFBookmark_GetTitle(IntPtr bookmark, byte[] buffer, uint buflen);
    [DllImport("pdfium.dll")] public static extern int FPDF_GetSignatureCount(IntPtr doc);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDF_GetSignatureObject(IntPtr doc, int index);
    [DllImport("pdfium.dll")] public static extern uint FPDFSignatureObj_GetContents(IntPtr sig, byte[] buffer, uint length);
    [DllImport("pdfium.dll")] public static extern uint FPDFSignatureObj_GetByteRange(IntPtr sig, int[] buffer, uint length);
    [DllImport("pdfium.dll")] public static extern uint FPDFSignatureObj_GetSubFilter(IntPtr sig, byte[] buffer, uint length);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFDOC_InitFormFillEnvironment(IntPtr doc, IntPtr formInfo);
    [DllImport("pdfium.dll")] public static extern void FPDFDOC_ExitFormFillEnvironment(IntPtr h);
    [DllImport("pdfium.dll")] public static extern uint FPDFAnnot_GetFormFieldName(IntPtr h, IntPtr annot, byte[] buffer, uint buflen);
    [DllImport("pdfium.dll")] public static extern uint FPDFAnnot_GetFormFieldValue(IntPtr h, IntPtr annot, byte[] buffer, uint buflen);
    [DllImport("pdfium.dll")] public static extern int FPDFAnnot_GetFormFieldType(IntPtr h, IntPtr annot);
    [DllImport("pdfium.dll")] public static extern int FPDFAnnot_GetFormFieldFlags(IntPtr h, IntPtr annot);
    [DllImport("pdfium.dll")] public static extern int FPDFAnnot_IsChecked(IntPtr h, IntPtr annot);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFBitmap_Create(int w, int h, int alpha);
    [DllImport("pdfium.dll")] public static extern void FPDFBitmap_FillRect(IntPtr bmp, int l, int t, int w, int h, uint color);
    [DllImport("pdfium.dll")] public static extern void FPDF_RenderPageBitmap(IntPtr bmp, IntPtr page, int x, int y, int sx, int sy, int rotate, int flags);
    [DllImport("pdfium.dll")] public static extern IntPtr FPDFBitmap_GetBuffer(IntPtr bmp);
    [DllImport("pdfium.dll")] public static extern int FPDFBitmap_GetStride(IntPtr bmp);
    [DllImport("pdfium.dll")] public static extern void FPDFBitmap_Destroy(IntPtr bmp);

    public static string Wide(byte[] buf, uint lenBytes) {
        if (lenBytes < 2) { return ""; }
        int n = (int)Math.Min(lenBytes, (uint)buf.Length) - 2;
        if (n <= 0) { return ""; }
        return Encoding.Unicode.GetString(buf, 0, n);
    }

    public static string PageText(IntPtr page) {
        IntPtr tp = FPDFText_LoadPage(page);
        if (tp == IntPtr.Zero) { return ""; }
        int count = FPDFText_CountChars(tp);
        string text = "";
        if (count > 0) {
            byte[] buf = new byte[(count + 1) * 2];
            int written = FPDFText_GetText(tp, 0, count, buf);
            if (written > 1) { text = Encoding.Unicode.GetString(buf, 0, (written - 1) * 2); }
        }
        FPDFText_ClosePage(tp);
        return text;
    }

    // Render a page to 0xAARRGGBB pixels on white and return (dark pixel count, total).
    public static int[] InkStats(IntPtr page, int width, int height, int x0, int y0, int x1, int y1) {
        IntPtr bmp = FPDFBitmap_Create(width, height, 0);
        FPDFBitmap_FillRect(bmp, 0, 0, width, height, 0xFFFFFFFF);
        FPDF_RenderPageBitmap(bmp, page, 0, 0, width, height, 0, 0x01);  // FPDF_ANNOT
        IntPtr buffer = FPDFBitmap_GetBuffer(bmp);
        int stride = FPDFBitmap_GetStride(bmp);
        int dark = 0; int total = 0;
        for (int y = Math.Max(0, y0); y < Math.Min(height, y1); y++) {
            for (int x = Math.Max(0, x0); x < Math.Min(width, x1); x++) {
                int b = Marshal.ReadByte(buffer, y * stride + x * 4);
                int g = Marshal.ReadByte(buffer, y * stride + x * 4 + 1);
                int r = Marshal.ReadByte(buffer, y * stride + x * 4 + 2);
                total++;
                if (r + g + b < 384) { dark++; }
            }
        }
        FPDFBitmap_Destroy(bmp);
        return new int[] { dark, total };
    }
}
"@
}

$script:PdfiumDll = if ($script:PdfiumDll) { $script:PdfiumDll } else { Join-Path $env:LOCALAPPDATA 'Programs\PDFXI\pdfium.dll' }
if (-not $script:PdfiumLoaded) {
    $handle = [RvPdf]::LoadLibrary($script:PdfiumDll)
    if ($handle -eq [IntPtr]::Zero) { throw "could not load $script:PdfiumDll" }
    [RvPdf]::FPDF_InitLibrary()
    $script:PdfiumLoaded = $true
}

$script:AnnotNames = @{ 1='Text'; 2='Link'; 3='FreeText'; 4='Line'; 5='Square'; 6='Circle'; 7='Polygon'; 8='PolyLine'; 9='Highlight'; 10='Underline'; 11='Squiggly'; 12='StrikeOut'; 13='Stamp'; 14='Caret'; 15='Ink'; 16='Popup'; 17='FileAttachment'; 18='Sound'; 19='Movie'; 20='Widget'; 21='Screen'; 27='Redact' }
$script:FieldNames = @{ 0='unknown'; 1='pushbutton'; 2='checkbox'; 3='radio'; 4='combobox'; 5='listbox'; 6='text'; 7='signature' }

function Open-Pdfium {
    # Returns a hashtable with Doc (IntPtr), Pin (GCHandle), Bytes, Error.
    param([Parameter(Mandatory = $true)][string]$Path, [string]$Password = $null)
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $pin = [System.Runtime.InteropServices.GCHandle]::Alloc($bytes, [System.Runtime.InteropServices.GCHandleType]::Pinned)
    $doc = [RvPdf]::FPDF_LoadMemDocument($pin.AddrOfPinnedObject(), $bytes.Length, $Password)
    $err = 0
    if ($doc -eq [IntPtr]::Zero) { $err = [RvPdf]::FPDF_GetLastError() }
    @{ Doc = $doc; Pin = $pin; Bytes = $bytes; Error = $err }
}

function Close-Pdfium {
    param($Handle)
    if ($Handle.Doc -ne [IntPtr]::Zero) { [RvPdf]::FPDF_CloseDocument($Handle.Doc) }
    $Handle.Pin.Free()
}

function Pdf-Report {
    # Print what an independent reader (PDFium) sees in a file.
    param([Parameter(Mandatory = $true)][string]$Path, [string]$Password = $null, [int]$TextChars = 160, [int]$MaxPages = 6)
    $errNames = @{ 1='unknown'; 2='file'; 3='format'; 4='PASSWORD required/incorrect'; 5='security'; 6='page' }
    $h = Open-Pdfium -Path $Path -Password $Password
    try {
        if ($h.Doc -eq [IntPtr]::Zero) { "PDFIUM $([IO.Path]::GetFileName($Path)): LOAD FAILED error=$($h.Error) ($($errNames[[int]$h.Error]))"; return }
        $doc = $h.Doc
        $pages = [RvPdf]::FPDF_GetPageCount($doc)
        $rev = [RvPdf]::FPDF_GetSecurityHandlerRevision($doc)
        $perm = [RvPdf]::FPDF_GetDocPermissions($doc)
        "PDFIUM $([IO.Path]::GetFileName($Path)): bytes=$($h.Bytes.Length) pages=$pages securityRevision=$rev permissions=0x$('{0:X8}' -f $perm)"
        $buf = New-Object byte[] 4096
        $meta = @()
        foreach ($tag in 'Title', 'Author', 'Subject', 'Keywords', 'Creator', 'Producer') {
            $n = [RvPdf]::FPDF_GetMetaText($doc, $tag, $buf, 4096)
            $v = [RvPdf]::Wide($buf, $n)
            if ($v) { $meta += "$tag='$v'" }
        }
        "  info: $(if ($meta.Count) { $meta -join '; ' } else { '(none)' })"
        # bookmarks (top level)
        $titles = @()
        $bm = [RvPdf]::FPDFBookmark_GetFirstChild($doc, [IntPtr]::Zero)
        $guard = 0
        while ($bm -ne [IntPtr]::Zero -and $guard -lt 50) {
            $n = [RvPdf]::FPDFBookmark_GetTitle($bm, $buf, 4096)
            $titles += [RvPdf]::Wide($buf, $n)
            $bm = [RvPdf]::FPDFBookmark_GetNextSibling($doc, $bm); $guard++
        }
        "  bookmarks(top)=$($titles.Count)$(if ($titles.Count) { ': ' + ($titles -join ' | ') })"
        "  signatures=$([RvPdf]::FPDF_GetSignatureCount($doc))"
        # form environment for field queries
        $fi = [System.Runtime.InteropServices.Marshal]::AllocHGlobal(2048)
        for ($i = 0; $i -lt 2048; $i += 8) { [System.Runtime.InteropServices.Marshal]::WriteInt64($fi, $i, 0) }
        [System.Runtime.InteropServices.Marshal]::WriteInt32($fi, 0, 1)
        $form = [RvPdf]::FPDFDOC_InitFormFillEnvironment($doc, $fi)
        for ($p = 0; $p -lt [Math]::Min($pages, $MaxPages); $p++) {
            $page = [RvPdf]::FPDF_LoadPage($doc, $p)
            if ($page -eq [IntPtr]::Zero) { "  page $($p+1): LOAD FAILED"; continue }
            $w = [RvPdf]::FPDF_GetPageWidth($page); $ht = [RvPdf]::FPDF_GetPageHeight($page); $rot = [RvPdf]::FPDFPage_GetRotation($page)
            $text = ([RvPdf]::PageText($page) -replace '\s+', ' ').Trim()
            $shown = if ($text.Length -gt $TextChars) { $text.Substring(0, $TextChars) + '...' } else { $text }
            "  page $($p+1): {0:0}x{1:0} rotate={2} textChars={3} text='{4}'" -f $w, $ht, ($rot * 90), $text.Length, $shown
            $ac = [RvPdf]::FPDFPage_GetAnnotCount($page)
            for ($a = 0; $a -lt $ac; $a++) {
                $annot = [RvPdf]::FPDFPage_GetAnnot($page, $a)
                $st = [RvPdf]::FPDFAnnot_GetSubtype($annot)
                $name = if ($script:AnnotNames.ContainsKey($st)) { $script:AnnotNames[$st] } else { "subtype$st" }
                $n = [RvPdf]::FPDFAnnot_GetStringValue($annot, 'Contents', $buf, 4096); $contents = [RvPdf]::Wide($buf, $n)
                $flags = [RvPdf]::FPDFAnnot_GetFlags($annot)
                $line = "    annot[$a] $name flags=$flags"
                if ($contents) { $line += " contents='$contents'" }
                if ($st -eq 20 -and $form -ne [IntPtr]::Zero) {
                    $n = [RvPdf]::FPDFAnnot_GetFormFieldName($form, $annot, $buf, 4096); $fname = [RvPdf]::Wide($buf, $n)
                    $n = [RvPdf]::FPDFAnnot_GetFormFieldValue($form, $annot, $buf, 4096); $fval = [RvPdf]::Wide($buf, $n)
                    $ft = [RvPdf]::FPDFAnnot_GetFormFieldType($form, $annot)
                    $ff = [RvPdf]::FPDFAnnot_GetFormFieldFlags($form, $annot)
                    $chk = [RvPdf]::FPDFAnnot_IsChecked($form, $annot)
                    $line += " field='$fname' type=$($script:FieldNames[[int]$ft]) Ff=$ff value='$fval' checked=$([bool]$chk)"
                }
                $line
                [RvPdf]::FPDFPage_CloseAnnot($annot)
            }
            [RvPdf]::FPDF_ClosePage($page)
        }
        if ($form -ne [IntPtr]::Zero) { [RvPdf]::FPDFDOC_ExitFormFillEnvironment($form) }
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($fi)
    } finally { Close-Pdfium $h }
}

function Pdf-Text {
    # All page text an independent reader extracts, joined by newlines.
    param([Parameter(Mandatory = $true)][string]$Path, [string]$Password = $null)
    $h = Open-Pdfium -Path $Path -Password $Password
    try {
        if ($h.Doc -eq [IntPtr]::Zero) { return $null }
        $out = @()
        $pages = [RvPdf]::FPDF_GetPageCount($h.Doc)
        for ($p = 0; $p -lt $pages; $p++) {
            $page = [RvPdf]::FPDF_LoadPage($h.Doc, $p)
            if ($page -ne [IntPtr]::Zero) { $out += [RvPdf]::PageText($page); [RvPdf]::FPDF_ClosePage($page) }
        }
        $out -join "`n"
    } finally { Close-Pdfium $h }
}

function Find-RawBytes {
    # Search a file's raw bytes for a needle as ASCII, UTF-16LE, UTF-16BE and hex-string (upper/lower).
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)][string]$Needle)
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $latin = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $forms = [ordered]@{
        'ascii'    = $Needle
        'utf16le'  = [System.Text.Encoding]::GetEncoding(28591).GetString([System.Text.Encoding]::Unicode.GetBytes($Needle))
        'utf16be'  = [System.Text.Encoding]::GetEncoding(28591).GetString([System.Text.Encoding]::BigEndianUnicode.GetBytes($Needle))
        'hex-upper' = (([System.Text.Encoding]::ASCII.GetBytes($Needle) | ForEach-Object { '{0:X2}' -f $_ }) -join '')
        'hex-lower' = (([System.Text.Encoding]::ASCII.GetBytes($Needle) | ForEach-Object { '{0:x2}' -f $_ }) -join '')
        'hex-utf16be-upper' = (([System.Text.Encoding]::BigEndianUnicode.GetBytes($Needle) | ForEach-Object { '{0:X2}' -f $_ }) -join '')
    }
    $hits = @()
    foreach ($k in $forms.Keys) {
        $count = ([regex]::Matches($latin, [regex]::Escape($forms[$k]))).Count
        $hits += "$k=$count"
    }
    "RAW '$Needle' in $([IO.Path]::GetFileName($Path)): $($hits -join ' ')"
}

function Get-InflatedStreams {
    # Inflate every FlateDecode stream found by scanning for 'stream'...'endstream' and
    # return the concatenated decoded text (Latin-1), for content searches in compressed data.
    param([Parameter(Mandatory = $true)][string]$Path)
    $bytes = [System.IO.File]::ReadAllBytes($Path)
    $latin = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $sb = New-Object System.Text.StringBuilder
    $rx = [regex]'stream\r?\n'
    $pos = 0; $count = 0; $ok = 0
    foreach ($m in $rx.Matches($latin)) {
        $start = $m.Index + $m.Length
        $end = $latin.IndexOf('endstream', $start)
        if ($end -lt 0) { continue }
        $count++
        $len = $end - $start
        if ($len -lt 3) { continue }
        try {
            # zlib: skip 2-byte header
            $ms = New-Object System.IO.MemoryStream(, $bytes[($start + 2)..($end - 1)])
            $ds = New-Object System.IO.Compression.DeflateStream($ms, [System.IO.Compression.CompressionMode]::Decompress)
            $outMs = New-Object System.IO.MemoryStream
            $ds.CopyTo($outMs)
            [void]$sb.Append([System.Text.Encoding]::GetEncoding(28591).GetString($outMs.ToArray()))
            [void]$sb.Append("`n")
            $ok++
        } catch { }
    }
    @{ Text = $sb.ToString(); Streams = $count; Inflated = $ok }
}
