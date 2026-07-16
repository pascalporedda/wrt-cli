#compdef wrt

_wrt_worktrees() {
  local -a names
  names=(${(f)"$(wrt ls 2>/dev/null | awk 'NF && $1 !~ /^\\(/ {print $1}')"})
  _describe -t worktrees 'worktree' names
}

_wrt() {
  local context state state_descr line

  _arguments -C \
    '1:command:->cmds' \
    '*::arg:->args'

  case $state in
    cmds)
      _values 'command' \
        'help[Print usage]' \
        'init[Generate shared managed-root config]' \
        'clone[Clone into a managed root]' \
        'root[Manage bare-root environments]' \
        'new[Create a new worktree]' \
        'add[Create a new worktree]' \
        'db[Run database utilities]' \
        'ls[List tracked worktrees]' \
        'path[Print worktree path]' \
        'env[Print exports for a worktree]' \
        'rm[Remove a worktree]' \
        'remove[Remove a worktree]' \
        'prune[Prune git worktrees and state]' \
        'housekeeping[Clean old unused branches]' \
        'run[Run a command in a worktree]' \
        'completions[Generate zsh completions]'
      return
      ;;
    args)
      case $words[2] in
        rm|remove)
          _arguments -C \
            '1:worktree:_wrt_worktrees' \
            '--force[Force remove]' \
            '--delete-branch[Delete branch]'
          return
          ;;
        path)
          _arguments '1:worktree:_wrt_worktrees'
          return
          ;;
        env)
          _arguments '1::worktree:_wrt_worktrees'
          return
          ;;
        run)
          _arguments -C '1:worktree:_wrt_worktrees' '*::command:_command_names -e'
          return
          ;;
        db)
          _arguments -C \
            '1::worktree:_wrt_worktrees' \
            '2:action:(reset seed migrate)' \
            '--print[Print the command that would be run and exit]' \
            '--yes[Skip interactive prompts (reset only)]'
          return
          ;;
        new|add)
          _arguments -C \
            '1:name:' \
            '--from=[Start ref]:ref:' \
            '--branch=[Branch name]:branch:' \
            '--install=[Install deps]:mode:(auto true false)' \
            '--supabase=[Supabase]:mode:(auto shared isolated none true false)' \
            '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
            '--db=[DB setup]:mode:(auto true false)' \
            '--cd[Print cd snippet]'
          return
          ;;
        clone)
          _arguments -C \
            '1:git repository:_files' \
            '--root=[Managed root directory]:directory:_files -/' \
            '--main=[Main branch]:branch:' \
            '--install=[Install deps]:mode:(auto true false)' \
            '--supabase=[Supabase]:mode:(auto true false)' \
            '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
            '--db=[DB setup]:mode:(auto true false)'
          return
          ;;
        init)
          _arguments -C \
            '--force[Overwrite existing .wrt.json]' \
            '--print[Print config and exit]' \
            '--model=[Codex model]:model:'
          return
          ;;
        root)
          case $words[3] in
            init)
              _arguments -C \
                '1:source:_files' \
                '--root=[Managed root directory]:directory:_files -/' \
                '--main=[Main branch]:branch:' \
                '--install=[Install deps]:mode:(auto true false)' \
                '--supabase=[Supabase]:mode:(auto true false)' \
                '--supabase-config=[Repo-relative Supabase config path]:file:_files' \
                '--db=[DB setup]:mode:(auto true false)'
              return
              ;;
            status)
              return
              ;;
            *)
              _values 'root command' \
                'init[Create a managed root]' \
                'status[Print managed-root status]'
              return
              ;;
          esac
          ;;
        housekeeping)
          _arguments '--apply[Delete candidates]'
          return
          ;;
        completions)
          _arguments '1:shell:(zsh)'
          return
          ;;
      esac
      ;;
  esac
}

_wrt "$@"
