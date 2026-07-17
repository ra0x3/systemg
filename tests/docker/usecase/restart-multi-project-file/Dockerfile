FROM sysg-usecase-base

COPY tests/docker/usecase/restart-multi-project-file/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-multi-project-file/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
