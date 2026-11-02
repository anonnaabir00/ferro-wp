<?php
/**
 * Adminer 6 plugin: auto-login to the ferro site's database.
 * Without a picked site every method defers to stock Adminer behavior,
 * so the normal login form stays reachable for other databases.
 */
class AdminerFerro extends Adminer\Plugin
{
    private $creds;

    function __construct()
    {
        $this->creds = ferro_adminer_creds();
    }

    function credentials()
    {
        if (!$this->creds) {
            return null;
        }
        return [$this->creds['server'], $this->creds['user'], $this->creds['pass']];
    }

    function database()
    {
        return $this->creds ? $this->creds['db'] : null;
    }

    function login($login, $password)
    {
        if ($this->creds) {
            return true; // credentials came from the daemon, not the user
        }
        return null;
    }

    function loginFormField($name, $heading, $value)
    {
        if (!$this->creds) {
            return null; // stock form
        }
        $values = [
            'driver' => 'server',
            'server' => $this->creds['server'],
            'username' => $this->creds['user'],
            'password' => $this->creds['pass'],
            'db' => $this->creds['db'],
        ];
        if (!isset($values[$name])) {
            return null;
        }
        $out = $heading
            . '<input type="hidden" name="auth[' . $name . ']" value="'
            . htmlspecialchars($values[$name], ENT_QUOTES) . '">';
        if ($name === 'server') {
            $site = htmlspecialchars($this->creds['site'], ENT_QUOTES);
            $out .= Adminer\script(
                "window.addEventListener('load', function () {"
                . " var f = document.querySelector('form[method=\"post\"]'); if (f) { f.submit(); }"
                . " });"
            );
            $out .= '<p class="ferro-note" style="grid-column:1/-1">ferro: signing in to <b>'
                . $site . '</b>… (<a href="?ferro=clear">pick a different site</a>)</p>';
        }
        return $out;
    }
}
