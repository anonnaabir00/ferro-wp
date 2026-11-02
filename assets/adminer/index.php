<?php
/**
 * Ferro WP Adminer bootstrap: site picker + auto-login.
 *
 * Flow:
 *   /                 -> site picker (or straight into Adminer when a site cookie exists)
 *   /?site=<name>     -> remember site (httpOnly cookie), redirect to /
 *   /?ferro=other     -> forget site, show the stock Adminer login form
 *   /?ferro=clear     -> forget site, back to the picker
 *
 * Credentials always come from the ferro daemon over its Unix socket —
 * never from the URL or the page.
 */

$home = getenv('HOME') ?: '';
$bridge = __DIR__ . '/ferro-bridge.php';
if (!file_exists($bridge)) {
    http_response_code(500);
    exit('ferro: ferro-bridge.php is missing — re-run scripts/fetch-adminer.sh');
}
require $bridge;

// --- pick a site -----------------------------------------------------
if (isset($_GET['site'])) {
    $name = (string) $_GET['site'];
    if (!preg_match('/^[a-z0-9-]{1,63}$/', $name)) {
        http_response_code(400);
        exit('ferro: invalid site name');
    }
    $info = ferro_rpc('site.info', ['name' => $name]);
    if (!$info || empty($info['site']['db_name'])) {
        http_response_code(404);
        exit("ferro: no site named '$name' (or it has no database)");
    }
    $secure = !empty($_SERVER['HTTPS']) && $_SERVER['HTTPS'] !== 'off';
    setcookie('ferro_site', $name, [
        'path' => '/',
        'secure' => $secure,
        'httponly' => true,
        'samesite' => 'Lax',
    ]);
    header('Location: ' . ferro_clean_url());
    exit;
}

if (isset($_GET['ferro'])) {
    // Forget the stored site; "other" goes on to the stock login form.
    setcookie('ferro_site', '', ['path' => '/', 'expires' => 1]);
    if ($_GET['ferro'] === 'other') {
        header('Location: ' . ferro_clean_url() . '?username=');
    } else {
        header('Location: ' . ferro_clean_url());
    }
    exit;
}

// --- picker -----------------------------------------------------------
$has_cookie = !empty($_COOKIE['ferro_site']);
$stock_login = isset($_GET['username']) || isset($_GET['server']) || !empty($_POST['auth']);
if (!$has_cookie && !$stock_login) {
    $sites = ferro_rpc('site.list') ?: [];
    $sites = array_values(array_filter($sites, function ($s) {
        return ($s['name'] ?? '') !== 'adminer' && !empty($s['db_name']);
    }));
    http_response_code(200);
    header('Content-Type: text/html; charset=utf-8');
    echo '<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">';
    echo '<title>ferro — databases</title>';
    echo '<style>
        :root { color-scheme: light dark; }
        body { font-family: -apple-system, sans-serif; margin: 3rem auto; max-width: 34rem; padding: 0 1rem; }
        h1 { font-size: 1.3rem; } a.site { display: block; padding: .6rem .8rem; margin: .35rem 0;
        border: 1px solid rgba(127,127,127,.35); border-radius: .5rem; text-decoration: none; color: inherit; }
        a.site:hover { border-color: #2271b1; } .db { opacity: .6; font-size: .85em; }
        footer { margin-top: 2rem; opacity: .7; font-size: .9rem; }
    </style>';
    echo '<h1>ferro — pick a database</h1>';
    if (!$sites) {
        echo '<p>No sites with a database yet. Create one with <code>ferro new &lt;name&gt;</code>.</p>';
    }
    foreach ($sites as $s) {
        $name = htmlspecialchars($s['name'], ENT_QUOTES);
        $db = htmlspecialchars($s['db_name'], ENT_QUOTES);
        echo "<a class='site' href='?site=$name'><b>$name</b> <span class='db'>$db</span></a>";
    }
    echo "<footer><a href='?ferro=other'>Log in to another database…</a></footer>";
    exit;
}

// --- hand over to Adminer ---------------------------------------------
function adminer_object()
{
    require_once __DIR__ . '/ferro-plugin.php';
    return new Adminer\Plugins([new AdminerFerro()]);
}

require __DIR__ . '/adminer.php';
