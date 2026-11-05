<?php
/**
 * ferro <-> Adminer bridge: talk to the ferro daemon over its Unix socket.
 */

function ferro_rpc(string $method, array $params = []): ?array
{
    $home = getenv('HOME');
    if (!$home) {
        return null;
    }
    $sock = $home . '/Library/Application Support/ferro/ferrod.sock';
    if (!file_exists($sock)) {
        return null;
    }
    $fp = @fsockopen('unix://' . $sock, 0, $errno, $errstr, 2.0);
    if (!$fp) {
        return null;
    }
    fwrite($fp, json_encode(['id' => 1, 'method' => $method, 'params' => $params]) . "\n");
    $line = fgets($fp);
    fclose($fp);
    $response = json_decode($line ?: '', true);
    return $response['result'] ?? null;
}

/** Current URL without its query string. */
function ferro_clean_url(): string
{
    return strtok($_SERVER['REQUEST_URI'] ?? '/', '?') ?: '/';
}

/**
 * The site named by the request host: <site>.localhost -> <site>.
 * Adminer is served per-site under /__adminer/, so the host is the
 * only site context.
 */
function ferro_adminer_site(): ?string
{
    $host = strtolower((string) ($_SERVER['HTTP_HOST'] ?? ''));
    $host = preg_replace('/:\d+$/', '', $host);
    $suffix = '.localhost';
    if ($host === '' || !str_ends_with($host, $suffix)) {
        return null;
    }
    $name = substr($host, 0, -strlen($suffix));
    return preg_match('/^[a-z0-9-]{1,63}$/', $name) ? $name : null;
}

/**
 * DB credentials for the host-named site, fresh from the daemon.
 * Never read from the URL, never written to disk by this file.
 * An explicit stock-login request (?username= / ?server=) disables
 * auto-login so the normal Adminer form stays reachable.
 */
function ferro_adminer_creds(): ?array
{
    if (isset($_GET['username']) || isset($_GET['server'])) {
        return null;
    }
    $site = ferro_adminer_site();
    if (!$site) {
        return null;
    }
    $info = ferro_rpc('site.info', ['name' => $site]);
    if (!$info || empty($info['site']['db_user'])) {
        return null;
    }
    $port = $info['mysql_port'] ?? 3307;
    return [
        'server' => '127.0.0.1:' . $port,
        'user' => $info['site']['db_user'],
        'pass' => $info['site']['db_pass'],
        'db' => $info['site']['db_name'],
        'site' => $site,
    ];
}
