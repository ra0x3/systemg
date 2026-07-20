FROM sysg-usecase-base

COPY tests/docker/usecase/abuse-stop-start-race/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/abuse-stop-start-race/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
