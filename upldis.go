/* --- LICENCE ---
* UPLDIS is licenced under the GNU GPLv3
*
* This program is free software: you can redistribute it and/or modify
* it under the terms of the GNU General Public License as published by
* the Free Software Foundation, either version 3 of the License, or
* (at your option) any later version.
*
* This program is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
* GNU General Public License for more details.
*
* You should have received a copy of the GNU General Public License
* along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

package main

import (
	"bytes"
	"os"
	"path"

	"fmt"
	"html/template"
	"io/ioutil"
	"log"
	"math/rand"
	"net/http"
	"os/exec"
	"strings"
	"time"

	"github.com/gorilla/mux"
	md "github.com/shurcooL/github_flavored_markdown"
)

/* --- config --- */
const (
	/* --- url settings ---  */
	formVal      = "p" // the value the upload form uses. ie; 'p=<-'
	minPasteSize = 16
	maxPasteSize = 1024 * 1024 * 1024                                               // 32 MB
	urlLength    = 4                                                                // charlength of the url
	urlCharset   = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789" // available characters the url can use

	/* --- database settings --- */
	basePath  = "pastes"          // base paste storage dir
	cacheSize = 128 * 1024 * 1024 // 128 MB

	/* --- server settings --- */
	useSSL      = true
	httpsPort   = 8443                 // ssl port
	sslCertPath = "cert/fullchain.cer" // ssl cert
	sslKeyPath  = "cert/upld.info.key" // ssl priv key
	httpPort    = 8080                 // http port
	bindAddress = ""                   // bind address
)

const htmlPrefix = `<!doctype html>
  <html>
  <head>
    <title>{{.BaseURL}}{{.SubDir}} - command line pastebin and more</title>
    <style>
      body, textarea, button, input {
        background-color: #000000;
        color: #fff;
        padding: 0;
        margin: 0;
        height: 100%;
        width: 100%;
        border-width: 0;
      }
      div {
        padding: 0;
        margin: 4px 0 5px;
      }
      textarea {
        background-color: #212121;
        height: 95vh;
      }
      button {
        background-color: #484848;
        width: 25%;
        height: 3vh;
        display: inline-block;
        border-width: 3px;
      }
      input {
        background-color: #484848;
        width: 75%;
        height: 3vh;
        display: inline-block;
      }
    </style>
  </head>
<body>
  <form action="{{.Scheme}}://{{.BaseURL}}" spellcheck="false" method="POST" accept-charset="UTF-8">
    <div>
      <input name="file" placeholder="(enter optional filename...)"/><button type="submit">paste to ipfs</button>
    </div>
    <textarea name="p">

 (delete this text to type a paste)

 `

/* if you host your own I'd appreciate a to mention comp.st
 *  Need to make the title dynamic etc */
const standardUsageText = `{{.BaseURL}}(1)                              UPLD.IS                              {{.BaseURL}}(1)
 
 NAME
     {{.BaseURL}} - no bullshit ipfs pastebin
 
 SYNOPSIS
     # File Upload
     curl {{.BaseURL}} -T &lt;file path&gt;
 
     # Command output
     &lt;command&gt; | curl {{.BaseURL}}{{.SubDir}} -T -

     # View help info
     curl {{.BaseURL}}
 
 DESCRIPTION
     A simple, no bullshit command line pastebin, that stores files on IPFS. Pastes are
     created using HTTP PUT, or POST requests. A url is returned, but you can also view
     the file with the ipfs hash/name.
 
 INSTALL
     Add this to your shell's .rc for an easy to use alias for uploading files. 
     
     alias upld_file='f(){ curl {{.BaseURL}} -T $1; unset -f f; }; f'
     alias upld_output='curl {{.BaseURL}} -T -'
 
 EXAMPLE
     $ ps -aux | curl {{.BaseURL}} -T -
       {{.Scheme}}://{{.BaseURL}}/QmbsN8cyhk4wpv29RKCf3ZrRZj7TWK3careKmv2btezbBu
     $ curl {{.BaseURL}} -T filename.png
       {{.Scheme}}://{{.BaseURL}}/<hash>/filename.png

     # ALIAS
     $ upld_file filename.go
       {{.Scheme}}://{{.BaseURL}}/<hash>/filename.go
     $ ps -aux | upld_output

 FILE VIEW
     Add '?md' to the paste url to parse a github flavored markdown file into an html 
     file. Add '?&lt;lang&gt' for line numbers and syntax
     highlighting. Available lexars (short notation) can be found at 
     http://pygments.org/docs/lexers/
 
 SEE ALSO
     {{.BaseURL}} is a free service brought to you by Ossian, (c) 2022
     Source is available at https://github.com/ozwaldorf/upld.is
 `

const htmlSuffix = `</textarea>
 </form>
 </body>
 </html>`

// errors n shit
type (
	pasteTooLarge struct{}
	pasteTooSmall struct{}
	pasteNotFound struct{}
	pygmentsError struct{}
)

func (e pasteTooLarge) Error() string {
	return fmt.Sprintf("paste too large (maximum size %d bytes)", maxPasteSize)
}
func (e pasteTooSmall) Error() string { return "paste too small" }
func (e pasteNotFound) Error() string { return "unknown ipfs hash, or not a file" }
func (e pygmentsError) Error() string {
	return "unknown pygements lexar shortcode. view available lexars at https://pygments.org/docs/lexers/"
}

func newID() string {
	urlID := make([]byte, urlLength)
	for i := range urlID {
		urlID[i] = urlCharset[rand.Intn(len(urlCharset))]
	}
	return string(urlID)
}

func readPaste(key string) (paste []byte, err error) {
	// Unnamed file (use regular ipfs hash)
	cmd := exec.Command("ipfs", "cat", key)
	paste, err = cmd.Output()
	if err != nil {
		err = pasteNotFound{}
	}
	return
}

func writePaste(name string, data []byte) (key string, err error) {
	if len(data) > maxPasteSize {
		err = pasteTooLarge{}
		return
	} else if len(data) < minPasteSize {
		err = pasteTooSmall{}
		return
	}

	temp_dir := path.Join("pastes", newID())
	if name != "" {
		if err := os.MkdirAll(temp_dir, 0755); err != nil {
			return "", err
		}
	}

	temp_file := path.Join(temp_dir, name) // temp_dir = file if unnamed
	f, err := os.Create(temp_file)
	if err != nil {
		return "", err
	}

	f.Write(data)
	f.Close()

	// Add to IPFS
	if name != "" {
		// Named file (use a dir to preserve filename)
		cmd := exec.Command("ipfs", "add", "-r", temp_dir)
		output, err := cmd.Output()
		if err != nil {
			return "", err
		}

		// Create a File URL and return
		lines := strings.Split(string(output[:]), "\n")
		words := strings.Split(lines[len(lines)-2], " ")
		key = fmt.Sprintf("%s/%s", words[1], name)
	} else {
		// Unnamed file (use regular ipfs hash)
		cmd := exec.Command("ipfs", "add", temp_file)
		output, err := cmd.Output()
		if err != nil {
			return "", err
		}

		words := strings.Split(string(output[:]), " ")
		key = words[1]
	}

	err = os.Remove(temp_dir)

	return
}

func Highlight(code string, lexer string, key string) (string, error) {
	cmd := exec.Command("pygmentize", "-l"+lexer, "-fhtml", "-O encoding=utf-8,full,style=native,linenos=table,title="+key) //construct and exec html lexar
	cmd.Stdin = strings.NewReader(code)
	var out bytes.Buffer
	cmd.Stdout = &out
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	err := cmd.Run()
	if err != nil {
		log.Printf(err.Error())
	}
	return out.String(), err
}

type handler struct{}

func (h *handler) read(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)

	if useSSL {
		w.Header().Add("Strict-Transport-Security", "max-age=63072000; includeSubDomains") //ssl lab bullshit
	}
	if vars["hash"] != "" {
		var key string
		if vars["file"] != "" {
			key = fmt.Sprintf("%s/%s", vars["hash"], vars["file"])
		} else {
			key = vars["hash"]
		}
		paste, err := readPaste(key)
		if err != nil {
			if _, ok := err.(pasteNotFound); ok {
				http.Error(w, "not found", http.StatusNotFound)
			} else {
				http.Error(w, err.Error(), http.StatusInternalServerError)
			}
			log.Printf("[ERROR] %s (%s)\n", key, err.Error())
			return
		}
		log.Printf("[READ ] %s\n", key)

		if req.URL.RawQuery != "" {
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			switch req.URL.RawQuery {
			case "md":
				paste = md.Markdown([]byte(paste))
			default:
				syntax, err := Highlight(string(paste), req.URL.RawQuery, key)
				if err == nil {
					paste = []byte(syntax)
				} else {
					fmt.Fprintf(w, "error: %s", pygmentsError{}.Error())
					return
				}
			}
		}

		fmt.Fprintf(w, "%s", paste)
		return
	}
}

func (h *handler) post(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)
	body := req.FormValue(formVal)

	key, err := writePaste(vars["file"], []byte(body))
	if err != nil {
		switch err.(type) {
		case pasteTooLarge, pasteTooSmall:
			http.Error(w, err.Error(), http.StatusNotAcceptable)
		default:
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		log.Printf("[ERROR] %s (error: %s)\n", vars["file"], err.Error())
		return
	}
	log.Printf("[WRITE] %s\n", key)
	var scheme string
	if req.TLS != nil {
		scheme = "https://"
	} else {
		scheme = "http://"
	}
	fmt.Fprintf(w, "%s%s/%s\n", scheme, req.Host, key)
	return
}

func (h *handler) put(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)

	body, err := ioutil.ReadAll(req.Body)
	if err != nil {
		fmt.Fprint(w, "an error occurred")
		return
	}

	key, err := writePaste(vars["file"], body)
	if err != nil {
		switch err.(type) {
		case pasteTooLarge, pasteTooSmall:
			http.Error(w, err.Error(), http.StatusNotAcceptable)
		default:
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		log.Printf("[ERROR] %s (error: %s)\n", vars["file"], err.Error())
		return
	}

	log.Printf("[WRITE] %s (%s)\n", vars["file"], key)

	var scheme string
	if req.TLS != nil {
		scheme = "https://"
	} else {
		scheme = "http://"
	}
	fmt.Fprintf(w, "%s%s/%s\n", scheme, req.Host, key)
	return
}

func (h *handler) usage(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")

	var usageText string

	agent := req.Header.Get("User-Agent")
	if agent[:4] != "curl" {
		usageText = strings.Join([]string{htmlPrefix, standardUsageText, htmlSuffix}, "")
	} else {
		usageText = standardUsageText
	}

	tmpl, err := template.New("usage").Parse(usageText)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	trailingSlash := (map[bool]string{true: "/", false: ""})[vars["dir"] != ""]
	subDir := trailingSlash + vars["dir"]
	baseURL := req.Host
	var scheme string
	if req.TLS != nil {
		scheme = "https"
	} else {
		scheme = "http"
	}
	data := struct {
		HTML    bool
		BaseURL string
		FormVal string
		Scheme  string
		SubDir  string
	}{false, baseURL, formVal, scheme, subDir}
	err = tmpl.Execute(w, data)
	if err != nil {
		log.Fatal(err)
	}
}

func newHandler() http.Handler {
	h := handler{}
	r := mux.NewRouter().StrictSlash(false)

	// certbot existing web server
	r.PathPrefix("/.well-known/").Handler(http.StripPrefix("/.well-known/", http.FileServer(http.Dir(".well-known"))))

	r.HandleFunc("/", h.usage).Methods("GET")

	r.HandleFunc("/{hash}", h.read).Methods("GET")
	r.HandleFunc("/{hash}/{file}", h.read).Methods("GET")

	r.HandleFunc("/", h.post).Methods("POST")
	r.HandleFunc("/{file}", h.put).Methods("PUT")
	r.HandleFunc("/", h.put).Methods("PUT")

	return r
}

func main() {
	rand.Seed(time.Now().UTC().UnixNano())

	http.Handle("/", newHandler())
	if useSSL {
		httpsAddr := fmt.Sprintf("%s:%d", bindAddress, httpsPort)
		go http.ListenAndServeTLS(httpsAddr, sslCertPath, sslKeyPath, nil) //goroutine ssl server alongside other shit
	}
	httpAddr := fmt.Sprintf("%s:%d", bindAddress, httpPort)
	fmt.Print(http.ListenAndServe(httpAddr, nil))
}
